//! The hub, running inside the desktop app.
//!
//! This is the "free" setup from the plan: [`tab_hub::Hub`] on a local
//! SQLite file (`hub.db`, next to `tab.db`). The hub's change log is the
//! source of truth, and `tab.db` is this device's copy (see
//! [`crate::store::hub_sync`]):
//!
//! - Every change to a synced table is queued in the same transaction and
//!   sent to the hub as soon as it commits, as a whole-row `Put` or
//!   `Delete`. A change the hub refuses is rolled back to the hub's version.
//! - The app then reads the hub's log from where it last got to, so other
//!   devices' changes land in `tab.db` (and the UI) the same way, and a
//!   fresh `tab.db` is rebuilt from the hub.
//! - The first launch copies everything already in `tab.db` into an empty
//!   hub; a `tab.db` meeting a hub that already has data takes the hub's
//!   rows and adds only what the hub doesn't have.
//!
//! The sync speaks the hub protocol (`Apply`, `GetChanges`), so the same
//! loop points at a hub in tab cloud once this computer has moved there
//! ([`LocalHub::move_to_cloud`], [`LocalHub::use_cloud`]): `tab.db` then
//! syncs with the cloud hub over its WebSocket ([`crate::cloud_link`])
//! instead of with `hub.db`, which keeps the copy it had at the move.
//!
//! The hub also listens for other devices on a localhost WebSocket
//! (`wss://127.0.0.1:<port>/hub`, TLS 1.3 with a pinned certificate and the
//! token in a header, binary Protobuf frames from
//! `tab-protocol`), and reports what it's doing for the Hub page in
//! Settings ([`LocalHub::status`]).

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use axum::Router;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, put};
use chrono::{DateTime, Utc};
use serde::Serialize;
use tab_domain::ops::Op;
use tab_hub::{ConnId, Handled, Hub};
use tab_protocol::pb::{self, client_envelope::Body as Req, server_envelope::Body as Res};
use tab_protocol::{ServerEnvelope, encode};
use tab_store::HubStore;
use tab_store::blob::{BlobMeta, BlobStore};
use tab_store::fs_blob::FsBlobStore;
use tab_store::sqlite::SqliteStore;
use tokio::sync::{broadcast, mpsc};

use crate::agents::{AgentHub, Decision};
use crate::cloud_link::CloudLink;
use crate::engine::{Change, EngineEvent};
use crate::live::{FileRef, LiveEvent, SteerCommand, attachment_path};
use crate::model::SessionStatus;
use crate::store::Store;

/// The desktop app's own connection to its hub.
const APP: ConnId = 0;
/// How often lapsed leases are expired.
const TICK: Duration = Duration::from_secs(5);
/// How often sync runs when nothing woke it, as a backstop.
const IDLE: Duration = Duration::from_secs(5);
/// Changes read from the hub per request.
const PULL: u32 = 500;
/// How often files nothing refers to are cleaned up.
const COLLECT: Duration = Duration::from_secs(60 * 60);
/// How long a file the cloud hub didn't have isn't asked for again.
const MISS_FOR: Duration = Duration::from_secs(60);

/// Who a frame is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum To {
    /// Every connection that said hello except this one (sync pushes).
    AllBut(ConnId),
    /// One connection (live events, steering).
    Only(ConnId),
}

pub struct LocalHub {
    hub: Mutex<Hub<SqliteStore>>,
    /// Files (attachments), beside `hub.db` in `hub-files/`.
    files: Mutex<FsBlobStore>,
    path: PathBuf,
    /// Frames for the sockets.
    pushes: broadcast::Sender<(To, Arc<Vec<u8>>)>,
    /// Frames for the app itself (live events, steering for sessions it runs).
    inbox: mpsc::UnboundedSender<ServerEnvelope>,
    inbox_rx: Mutex<Option<mpsc::UnboundedReceiver<ServerEnvelope>>>,
    /// Runs the sessions steered from other devices, once set.
    agents: OnceLock<Arc<AgentHub>>,
    /// Approval prompts from sessions other devices run, by id: their session.
    remote_approvals: Mutex<std::collections::HashMap<String, String>>,
    /// Sessions other devices run that the app is watching.
    watching: Mutex<std::collections::BTreeSet<String>>,
    next_conn: AtomicU64,
    next_request: AtomicU64,
    url: OnceLock<String>,
    /// The listener certificate's fingerprint, which devices pin.
    fingerprint: OnceLock<String>,
    /// Whether HTTP/3 (QUIC) is up next to the TCP listener.
    http3: OnceLock<bool>,
    /// This device's copy and the engine's event stream, once following.
    local: OnceLock<(Arc<Store>, broadcast::Sender<EngineEvent>)>,
    /// None where the OS won't say.
    sys: Mutex<Option<sysinfo::System>>,
    /// Woken when another device changes something.
    remote: tokio::sync::Notify,
    /// The cloud hub `tab.db` syncs with instead of `hub.db`, once moved.
    cloud: std::sync::RwLock<Option<Arc<CloudLink>>>,
    /// Held while syncing `tab.db`, so a move doesn't race the sync loop.
    syncing: Mutex<()>,
    /// `tab.db` belongs to a cloud hub that isn't connected yet: sync waits
    /// (changes queue up) rather than go to `hub.db`, whose sequence
    /// numbers stopped matching at the move.
    awaiting_cloud: std::sync::atomic::AtomicBool,
    /// Files the cloud hub is known to have.
    uploaded: Mutex<std::collections::HashSet<String>>,
    /// Files that couldn't go up yet; tried again on each sync.
    to_upload: Mutex<std::collections::BTreeSet<String>>,
    /// Files the cloud hub didn't have, and when it was asked.
    misses: Mutex<std::collections::HashMap<String, std::time::Instant>>,
}

/// How [`LocalHub::move_to_cloud`] went.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MoveOutcome {
    /// The cloud hub was empty and now has this hub's history.
    Moved { changes: u64 },
    /// The cloud hub already had work; this computer joined it.
    Joined { head: u64 },
}

/// What the Hub page in Settings shows.
#[derive(Debug, Clone, Serialize)]
pub struct HubStatusView {
    pub runs_on: String,
    pub protocol_version: u32,
    pub started_at: Option<DateTime<Utc>>,
    /// Where other devices connect, once the listener is up.
    pub url: Option<String>,
    /// The hub certificate's fingerprint (`sha256/<hex>`), which devices pin.
    pub fingerprint: Option<String>,
    /// HTTP/3 (QUIC) is up next to HTTP/2.
    pub http3: bool,
    pub path: String,
    pub head: u64,
    pub records: u64,
    pub edges: u64,
    pub live_leases: u64,
    pub storage_bytes: u64,
    /// Files kept for attachments, and their total size.
    pub files: u64,
    pub file_bytes: u64,
    pub connections: Vec<ConnectionView>,
    pub cpu_percent: Option<f64>,
    pub memory_bytes: Option<u64>,
    pub cpus: usize,
    /// Local changes not yet sent to the hub.
    pub pending: u64,
    /// The cloud hub this computer's work syncs with, once it has moved.
    /// The rest of this view describes this computer's own hub, which
    /// keeps its copy from the move.
    pub cloud_hub: Option<String>,
    /// Whether that cloud hub is connected right now.
    pub cloud_connected: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConnectionView {
    pub id: u64,
    pub device: String,
    pub client: String,
    pub address: String,
    pub connected_at: Option<DateTime<Utc>>,
    pub synced_to: u64,
    /// Changes it hasn't been sent yet.
    pub behind: u64,
    /// This app's own connection.
    pub this_app: bool,
}

fn time(t: Option<prost_types::Timestamp>) -> Option<DateTime<Utc>> {
    t.and_then(|t| DateTime::from_timestamp(t.seconds, t.nanos.max(0) as u32))
}

impl LocalHub {
    /// Opens (or creates) the hub's database and connects the app to it.
    pub fn open(path: &Path) -> anyhow::Result<Arc<Self>> {
        let store = SqliteStore::open(path).map_err(|e| anyhow::anyhow!("{e}"))?;
        let now = Utc::now();
        let mut hub = Hub::new(store, "local", now);
        hub.connect(APP, "this app", now);
        let head = hub.store().head().map_err(|e| anyhow::anyhow!("{e}"))?;
        let hello = Req::Hello(pb::Hello {
            device: "this-computer".into(),
            client: format!("tab-desktop/{}", env!("CARGO_PKG_VERSION")),
            since: head,
        });
        reply_ok(hub.handle(APP, 0, hello, now))?;
        let (pushes, _) = broadcast::channel(256);
        let (inbox, inbox_rx) = mpsc::unbounded_channel();
        let files = FsBlobStore::open(path.with_file_name("hub-files"))
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        Ok(Arc::new(Self {
            hub: Mutex::new(hub),
            files: Mutex::new(files),
            path: path.to_path_buf(),
            pushes,
            inbox,
            inbox_rx: Mutex::new(Some(inbox_rx)),
            agents: OnceLock::new(),
            remote_approvals: Default::default(),
            watching: Default::default(),
            next_conn: AtomicU64::new(1),
            next_request: AtomicU64::new(1),
            url: OnceLock::new(),
            fingerprint: OnceLock::new(),
            http3: OnceLock::new(),
            local: OnceLock::new(),
            sys: Mutex::new(sysinfo::System::new().ok()),
            remote: tokio::sync::Notify::new(),
            cloud: Default::default(),
            syncing: Mutex::new(()),
            awaiting_cloud: Default::default(),
            uploaded: Default::default(),
            to_upload: Default::default(),
            misses: Default::default(),
        }))
    }

    /// Sends what handling a request from `from` produced to everyone it's
    /// for (but not the reply).
    fn deliver(
        &self,
        from: ConnId,
        push: Option<&ServerEnvelope>,
        direct: Vec<(ConnId, ServerEnvelope)>,
    ) {
        if let Some(push) = push {
            let _ = self.pushes.send((To::AllBut(from), Arc::new(encode(push))));
        }
        for (to, env) in direct {
            if to == APP {
                let _ = self.inbox.send(env);
            } else {
                let _ = self.pushes.send((To::Only(to), Arc::new(encode(&env))));
            }
        }
    }

    /// Runs one request as the app, sending any push to other devices.
    fn request(&self, req: Req) -> anyhow::Result<pb::server_envelope::Body> {
        let id = self.next_request.fetch_add(1, Ordering::Relaxed);
        let handled = self.hub.lock().unwrap().handle(APP, id, req, Utc::now());
        self.deliver(APP, handled.push.as_ref(), handled.direct);
        match handled.reply.body {
            Some(pb::server_envelope::Body::Error(e)) => Err(anyhow::anyhow!("hub: {}", e.message)),
            Some(body) => Ok(body),
            None => Err(anyhow::anyhow!("hub: empty reply")),
        }
    }

    /// The cloud hub `tab.db` syncs with, if this computer has moved.
    pub fn cloud_link(&self) -> Option<Arc<CloudLink>> {
        self.cloud.read().unwrap().clone()
    }

    /// A request to the hub `tab.db` syncs with: the cloud one once moved,
    /// else this app's own. Blocks on the cloud's reply.
    fn up(&self, req: Req) -> anyhow::Result<pb::server_envelope::Body> {
        match self.cloud_link() {
            Some(link) => link.request(req),
            None => self.request(req),
        }
    }

    fn apply(&self, ops: &[Op]) -> anyhow::Result<()> {
        self.up(Req::Apply(pb::Apply {
            ops: ops.iter().map(Into::into).collect(),
        }))
        .map(|_| ())
    }

    /// A record's body in the hub `tab.db` syncs with.
    fn hub_record(&self, kind: &str, id: &str) -> anyhow::Result<Option<serde_json::Value>> {
        if self.cloud_link().is_some() {
            let Res::Record(r) = self.up(Req::GetRecord(pb::GetRecord {
                kind: kind.into(),
                id: id.into(),
            }))?
            else {
                anyhow::bail!("hub: unexpected reply to GetRecord");
            };
            return r
                .record
                .map(|r| serde_json::from_slice(&r.body_json))
                .transpose()
                .map_err(Into::into);
        }
        Ok(self
            .hub
            .lock()
            .unwrap()
            .store()
            .get(kind, id)
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .map(|r| r.body))
    }

    /// The head of the hub `tab.db` syncs with.
    fn up_head(&self) -> anyhow::Result<u64> {
        if self.cloud_link().is_none() {
            return self.head();
        }
        // Nothing is newer than this, so the reply is just the head.
        let Res::Changes(page) = self.up(Req::GetChanges(pb::GetChanges {
            since: i64::MAX as u64,
            limit: 1,
        }))?
        else {
            anyhow::bail!("hub: unexpected reply to GetChanges");
        };
        Ok(page.head)
    }

    /// The head of this app's own hub.
    fn head(&self) -> anyhow::Result<u64> {
        self.hub
            .lock()
            .unwrap()
            .store()
            .head()
            .map_err(|e| anyhow::anyhow!("{e}"))
    }

    /// Sends one batch from the outbox. Returns how many rows it handled.
    fn push_once(&self, store: &Store) -> anyhow::Result<usize> {
        let rows = store.hub_outbox_take(tab_domain::ops::MAX_BATCH)?;
        let Some(last) = rows.last().map(|r| r.seq) else {
            return Ok(0);
        };
        let mut ops = Vec::with_capacity(rows.len());
        let mut seen = std::collections::HashSet::new();
        let cloud = self.cloud_link().is_some();
        for r in rows.iter().rev() {
            // The same row queued twice carries the same contents: send it once.
            if !seen.insert((r.table.as_str(), r.key.as_str())) {
                continue;
            }
            // Skipping rows the hub already has saves log entries. Asking a
            // cloud hub would cost a round trip per row, so it gets them all.
            let old = if cloud {
                None
            } else {
                Some(self.hub_record(&r.table, &r.key)?)
            };
            match (&r.row, old) {
                (Some(row), Some(Some(old))) if *row == old => {}
                (None, Some(None)) => {}
                (Some(row), _) => ops.push(Op::Put {
                    kind: r.table.clone(),
                    id: r.key.clone(),
                    body: row.clone(),
                }),
                (None, _) => ops.push(Op::Delete {
                    kind: r.table.clone(),
                    id: r.key.clone(),
                }),
            }
        }
        ops.reverse();
        // The files these records mention go up first, so a device that
        // gets the record can fetch what it names.
        if let Some(link) = self.cloud_link() {
            let mut refs = std::collections::BTreeSet::new();
            for op in &ops {
                if let Op::Put { body, .. } = op {
                    refs.extend(tab_domain::blob::refs(body));
                }
            }
            refs.extend(std::mem::take(&mut *self.to_upload.lock().unwrap()));
            self.upload(&link, refs, false);
        }
        let mut refused = vec![];
        if !ops.is_empty() && self.apply(&ops).is_err() {
            // Find which ones the hub won't take; the rest go through.
            for op in &ops {
                if let Err(e) = self.apply(std::slice::from_ref(op)) {
                    let (kind, id) = match op {
                        Op::Put { kind, id, .. } | Op::Delete { kind, id } => (kind, id),
                        _ => continue,
                    };
                    tracing::warn!(error = %e, table = %kind, key = %id, "hub: refused a change; undoing it here");
                    refused.push((kind.clone(), id.clone()));
                }
            }
        }
        store.hub_outbox_done(last)?;
        // The hub is the source of truth: a change it refused is undone here.
        if !refused.is_empty() {
            let mut undo = Vec::with_capacity(refused.len());
            for (kind, id) in refused {
                let body = self.hub_record(&kind, &id)?;
                undo.push((kind, id, body));
            }
            let changed = store.hub_apply_remote(&undo)?;
            self.announce(store, &changed);
        }
        Ok(rows.len())
    }

    /// Sends the cloud hub the files in `hashes` this hub has and it lacks.
    /// One that fails is tried again on the next sync. With `ask`, first
    /// asks whether the cloud has each (for files that may have gone up
    /// from another device, or before this app started).
    fn upload(&self, link: &CloudLink, hashes: impl IntoIterator<Item = String>, ask: bool) {
        for hash in hashes {
            if self.uploaded.lock().unwrap().contains(&hash) {
                continue;
            }
            let bytes = match self.file_get(&hash) {
                Ok(Some(b)) => b,
                // Not on this computer: another device's, or swept.
                Ok(None) => continue,
                Err(e) => {
                    tracing::warn!(error = %e, "hub: couldn't read a file to upload");
                    continue;
                }
            };
            let done = match ask.then(|| link.has_file(&hash)) {
                Some(Ok(true)) => Ok(()),
                _ => link.put_file(bytes).and_then(|s| {
                    anyhow::ensure!(s.hash == hash, "cloud hub stored {} for {hash}", s.hash);
                    Ok(())
                }),
            };
            match done {
                Ok(()) => {
                    self.uploaded.lock().unwrap().insert(hash);
                }
                Err(e) => {
                    tracing::warn!(error = %e, "hub: couldn't upload a file; will retry");
                    self.to_upload.lock().unwrap().insert(hash);
                }
            }
        }
    }

    /// Sends the cloud hub every file this hub keeps that it lacks, in the
    /// background: files attached before the move, or while it was away.
    fn upload_all_soon(self: &Arc<Self>) {
        let Some(link) = self.cloud_link() else {
            return;
        };
        let me = self.clone();
        std::thread::spawn(move || {
            let mut after = String::new();
            loop {
                let page = match me.files.lock().unwrap().list(&after, 200) {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::warn!(error = %e, "hub: couldn't list files to upload");
                        return;
                    }
                };
                let Some(last) = page.last() else {
                    return;
                };
                after = last.hash.clone();
                me.upload(&link, page.into_iter().map(|m| m.hash), true);
            }
        });
    }

    /// Reads one page of the hub's log into `tab.db`. Returns whether
    /// there's more.
    fn pull_once(&self, store: &Store) -> anyhow::Result<bool> {
        let since = store.hub_pulled_to()?;
        let pb::server_envelope::Body::Changes(page) =
            self.up(Req::GetChanges(pb::GetChanges { since, limit: PULL }))?
        else {
            anyhow::bail!("hub: unexpected reply to GetChanges");
        };
        let Some(last) = page.changes.last().map(|c| c.seq) else {
            return Ok(false);
        };
        // Only the latest version of each row matters.
        let mut latest: Vec<(String, String, Option<serde_json::Value>)> = vec![];
        let mut at = std::collections::HashMap::new();
        for c in page.changes {
            let row = match c.op.and_then(|o| Op::try_from(o).ok()) {
                Some(Op::Put { kind, id, body }) => (kind, id, Some(body)),
                Some(Op::Delete { kind, id }) => (kind, id, None),
                _ => continue,
            };
            match at.get(&(row.0.clone(), row.1.clone())) {
                Some(&i) => latest[i] = row,
                None => {
                    at.insert((row.0.clone(), row.1.clone()), latest.len());
                    latest.push(row);
                }
            }
        }
        let changed = store.hub_apply_remote(&latest)?;
        store.hub_set_pulled_to(last)?;
        self.announce(store, &changed);
        Ok(page.more)
    }

    /// Tells the UI about rows the hub changed here.
    fn announce(&self, store: &Store, changed: &[(String, String)]) {
        let Some((_, events)) = self.local.get() else {
            return;
        };
        for (table, key) in changed {
            let first = || {
                serde_json::from_str::<Vec<serde_json::Value>>(key)
                    .ok()
                    .and_then(|k| k.into_iter().next())
                    .and_then(|v| v.as_str().map(str::to_string))
            };
            match table.as_str() {
                "items" => {
                    if let Some(item) = first().and_then(|id| store.get(&id).ok().flatten()) {
                        let _ = events.send(EngineEvent::ItemChanged {
                            change: Change::Updated,
                            item,
                        });
                    }
                }
                "sessions" => {
                    if let Some(session) =
                        first().and_then(|id| store.session_get(&id).ok().flatten())
                    {
                        let _ = events.send(EngineEvent::SessionChanged { session });
                    }
                }
                "session_messages" => {
                    if let Some(message) =
                        first().and_then(|uid| store.message_by_uid(&uid).ok().flatten())
                    {
                        let _ = events.send(EngineEvent::SessionMessage { message });
                    }
                }
                "item_context" => {
                    let item = store
                        .hub_row(table, key)
                        .ok()
                        .flatten()
                        .and_then(|r| r["item_id"].as_str().map(str::to_string));
                    if let Some(item_id) = item {
                        let _ = events.send(EngineEvent::ContextChanged { item_id });
                    }
                }
                _ => {}
            }
        }
    }

    /// Sends everything waiting, then reads everything new.
    fn sync(&self, store: &Store) -> anyhow::Result<()> {
        while self.push_once(store)? > 0 {}
        while self.pull_once(store)? {}
        Ok(())
    }

    /// Introduces `tab.db` to the hub the first time they meet.
    fn introduce(&self, store: &Store) -> anyhow::Result<()> {
        store.hub_sync_install()?;
        let head = self.up_head()?;
        if store.hub_pulled_to()? > head {
            tracing::warn!("hub: hub.db is behind this copy (replaced or reset); syncing again");
            store.hub_forget()?;
        }
        if store.hub_backfilled()? {
            return Ok(());
        }
        if head == 0 {
            // An empty hub: it starts with everything here.
            let n = store.hub_backfill_once()?;
            tracing::info!(rows = n, "hub: copying tab.db into the hub");
            return Ok(());
        }
        // The hub already has data: take its rows, then add what it lacks.
        while self.pull_once(store)? {}
        let mut missing = vec![];
        for (table, key) in store.hub_local_keys()? {
            if self.hub_record(&table, &key)?.is_none() {
                missing.push((table, key));
            }
        }
        tracing::info!(
            rows = missing.len(),
            "hub: joined a hub with data; adding what it lacked"
        );
        store.hub_queue(&missing)?;
        store.hub_mark_backfilled()?;
        Ok(())
    }

    /// Starts syncing `tab.db` with the hub: introduces them, then sends
    /// each local change as it commits and reads each change the hub gets.
    pub fn follow(self: &Arc<Self>, store: Arc<Store>, events: broadcast::Sender<EngineEvent>) {
        let _ = self.local.set((store.clone(), events.clone()));
        // The app's connection goes by this install's device id, which is
        // what its sessions name as where they run.
        match store.device_id() {
            Ok(device) => {
                let hello = Req::Hello(pb::Hello {
                    device,
                    client: format!("tab-desktop/{}", env!("CARGO_PKG_VERSION")),
                    since: self.head().unwrap_or(0),
                });
                if let Err(e) = self.request(hello) {
                    tracing::warn!(error = %e, "hub: couldn't say hello as this device");
                }
            }
            Err(e) => tracing::warn!(error = %e, "hub: no device id"),
        }
        self.run_live(store.clone(), events);
        let me = self.clone();
        tokio::spawn(async move {
            if !me.awaiting_cloud.load(Ordering::Relaxed)
                && let Err(e) = me.introduce(&store)
            {
                tracing::error!(error = %e, "hub: couldn't set up sync; staying local");
                return;
            }
            let commits = store.commits();
            loop {
                // On a blocking thread: SQLite, and the cloud's replies.
                let (m, st) = (me.clone(), store.clone());
                let done = tokio::task::spawn_blocking(move || {
                    let _turn = m.syncing.lock().unwrap();
                    if m.awaiting_cloud.load(Ordering::Relaxed) {
                        return Ok(());
                    }
                    m.sync(&st)
                })
                .await;
                match done {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => tracing::warn!(error = %e, "hub: sync failed; will retry"),
                    Err(e) => tracing::error!(error = %e, "hub: sync panicked; will retry"),
                }
                let cloud = me.cloud_link().map(|l| l.changed.clone());
                let cloud_changed = async {
                    match &cloud {
                        Some(n) => n.notified().await,
                        None => std::future::pending().await,
                    }
                };
                tokio::select! {
                    _ = commits.notified() => {}
                    _ = me.remote.notified() => {}
                    _ = cloud_changed => {}
                    _ = tokio::time::sleep(IDLE) => {}
                }
            }
        });
        let me = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(TICK).await;
                let push = me.hub.lock().unwrap().tick(Utc::now());
                match push {
                    Ok(Some(p)) => me.deliver(APP, Some(&p), vec![]),
                    Ok(None) => {}
                    Err(e) => tracing::warn!(error = %e, "hub: expiring leases failed"),
                }
            }
        });
    }

    /// This computer moved to tab cloud before: hold sync until
    /// [`LocalHub::use_cloud`] connects. Call before [`LocalHub::follow`].
    pub fn expect_cloud(&self) {
        self.awaiting_cloud.store(true, Ordering::Relaxed);
    }

    /// Says hello to a cloud hub as this device. Returns its head.
    fn cloud_hello(&self, link: &CloudLink, store: &Store) -> anyhow::Result<u64> {
        link.hello(pb::Hello {
            device: store.device_id()?,
            client: format!("tab-desktop/{}", env!("CARGO_PKG_VERSION")),
            since: store.hub_pulled_to()?,
        })
    }

    /// Moves this computer's work into the cloud hub behind `link`, then
    /// syncs `tab.db` with it from now on. Blocks; run it on a blocking
    /// thread.
    ///
    /// An empty cloud hub gets this hub's whole log (`Import`), with every
    /// change's sequence number, author and time, so `tab.db` carries on
    /// from where it was. A cloud hub that already has work (another
    /// computer moved first) is joined instead: `tab.db` takes its rows and
    /// adds what it lacks, as when meeting any hub with data. `hub.db`
    /// keeps its copy either way. `progress(done, total)` hears each page.
    pub fn move_to_cloud(
        self: &Arc<Self>,
        link: Arc<CloudLink>,
        store: &Store,
        mut progress: impl FnMut(u64, u64),
    ) -> anyhow::Result<MoveOutcome> {
        let _turn = self.syncing.lock().unwrap();
        anyhow::ensure!(
            self.cloud_link().is_none(),
            "already syncing with tab cloud"
        );
        // Everything waiting goes into this hub first, so it moves too.
        self.sync(store)?;
        let cloud_head = self.cloud_hello(&link, store)?;
        let outcome = if cloud_head == 0 {
            let total = self.head()?;
            let mut since = 0;
            progress(0, total);
            loop {
                let page = self
                    .hub
                    .lock()
                    .unwrap()
                    .store()
                    .changes_since(since, PULL as usize)
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
                let Some(last) = page.last().map(|a| a.seq) else {
                    break;
                };
                let Res::Imported(done) = link.request(Req::Import(pb::Import {
                    changes: page.iter().map(Into::into).collect(),
                }))?
                else {
                    anyhow::bail!("cloud hub: unexpected reply to Import");
                };
                anyhow::ensure!(
                    done.head == last,
                    "cloud hub is at {} after importing up to {last}",
                    done.head
                );
                since = last;
                progress(since, total);
            }
            MoveOutcome::Moved { changes: since }
        } else {
            store.hub_forget()?;
            MoveOutcome::Joined { head: cloud_head }
        };
        *self.cloud.write().unwrap() = Some(link);
        self.upload_all_soon();
        self.introduce(store)?;
        self.sync(store)?;
        Ok(outcome)
    }

    /// Brings this computer's work back from tab cloud into its own hub,
    /// and stops syncing with the cloud. Blocks; run it on a blocking
    /// thread.
    ///
    /// `tab.db` is a full copy of the cloud hub, so after one last sync it
    /// is what `hub.db` should hold: every synced row goes in as a new
    /// change, and anything `hub.db` still has from before the move that
    /// the cloud has since dropped is deleted. `hub.db`'s older changes
    /// aren't read back, so they can't undo newer ones. The cloud hub keeps
    /// its copy, and other computers syncing with it carry on.
    pub fn move_to_local(&self, store: &Store) -> anyhow::Result<u64> {
        let _turn = self.syncing.lock().unwrap();
        anyhow::ensure!(self.cloud_link().is_some(), "not syncing with tab cloud");
        self.sync(store)?;
        *self.cloud.write().unwrap() = None;
        self.awaiting_cloud.store(false, Ordering::Relaxed);
        let mut keys: std::collections::BTreeSet<(String, String)> =
            store.hub_local_keys()?.into_iter().collect();
        let tables: std::collections::HashSet<&str> = crate::store::hub_sync::SYNCED
            .iter()
            .map(|(t, _)| *t)
            .collect();
        let head = self.head()?;
        let mut since = 0;
        while since < head {
            let page = self
                .hub
                .lock()
                .unwrap()
                .store()
                .changes_since(since, PULL as usize)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            let Some(last) = page.last().map(|a| a.seq) else {
                break;
            };
            for a in &page {
                if let Op::Put { kind, id, .. } | Op::Delete { kind, id } = &a.op
                    && tables.contains(kind.as_str())
                {
                    keys.insert((kind.clone(), id.clone()));
                }
            }
            since = last;
        }
        let keys: Vec<_> = keys.into_iter().collect();
        store.hub_queue(&keys)?;
        store.hub_set_pulled_to(head)?;
        store.hub_mark_backfilled()?;
        self.sync(store)?;
        Ok(keys.len() as u64)
    }

    /// Syncs `tab.db` with the cloud hub behind `link` (this computer moved
    /// before). Blocks; run it on a blocking thread.
    pub fn use_cloud(self: &Arc<Self>, link: Arc<CloudLink>, store: &Store) -> anyhow::Result<()> {
        let _turn = self.syncing.lock().unwrap();
        self.cloud_hello(&link, store)?;
        *self.cloud.write().unwrap() = Some(link);
        self.upload_all_soon();
        self.awaiting_cloud.store(false, Ordering::Relaxed);
        self.introduce(store)?;
        self.sync(store)
    }

    /// Listens for other devices on localhost: TLS 1.3 only, with this
    /// install's own certificate (see [`crate::hub_tls`]). TCP carries
    /// HTTP/2 and HTTP/1.1 (the sync WebSocket, over either), and QUIC on
    /// the same port carries HTTP/3 for files, advertised with `Alt-Svc`.
    /// Requests authenticate with `Authorization: Bearer <token>` (or, for
    /// a browser's WebSocket, the subprotocol `tab.token.<token>`), never a
    /// token in the URL. Returns the sync URL.
    pub async fn serve(self: &Arc<Self>, port: u16, token: String) -> anyhow::Result<String> {
        let identity =
            crate::hub_tls::Identity::load_or_create(&self.path.with_file_name("hub-tls"))?;
        let mut listener = None;
        for p in port..port.saturating_add(20) {
            if let Ok(l) = tokio::net::TcpListener::bind(("127.0.0.1", p)).await {
                listener = Some(l);
                break;
            }
        }
        let listener = listener.ok_or_else(|| anyhow::anyhow!("no free port for the hub"))?;
        let addr = listener.local_addr()?;
        let token = Arc::new(token);
        let h3 = match self.serve_h3(addr, &identity, token.clone()) {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!(error = %e, "hub: HTTP/3 isn't available; serving HTTP/2 only");
                false
            }
        };
        let mut app = Router::new()
            // `any`: a WebSocket is a GET upgrade on HTTP/1.1, a CONNECT on HTTP/2.
            .route("/hub", axum::routing::any(upgrade))
            .route("/files", put(file_upload))
            .route("/files/{hash}", get(file_download))
            .layer(axum::extract::DefaultBodyLimit::max(
                tab_domain::blob::MAX_BLOB_BYTES + 1024,
            ))
            .with_state((self.clone(), token));
        if h3 {
            let alt =
                axum::http::HeaderValue::from_str(&format!("h3=\":{}\"; ma=86400", addr.port()))?;
            app = app.layer(axum::middleware::map_response(move |mut res: Response| {
                let alt = alt.clone();
                async move {
                    res.headers_mut().insert(axum::http::header::ALT_SVC, alt);
                    res
                }
            }));
        }
        let tls = tokio_rustls::TlsAcceptor::from(Arc::new(
            identity.server(&[crate::hub_tls::H2, crate::hub_tls::H1])?,
        ));
        tokio::spawn(async move {
            loop {
                let (tcp, peer) = match listener.accept().await {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!(error = %e, "hub: accept failed");
                        continue;
                    }
                };
                let (tls, app) = (tls.clone(), app.clone());
                tokio::spawn(async move {
                    let Ok(stream) = tls.accept(tcp).await else {
                        return;
                    };
                    let svc = hyper::service::service_fn(
                        move |mut req: http::Request<hyper::body::Incoming>| {
                            req.extensions_mut().insert(ConnectInfo(peer));
                            tower::ServiceExt::oneshot(app.clone(), req)
                        },
                    );
                    let mut builder = hyper_util::server::conn::auto::Builder::new(
                        hyper_util::rt::TokioExecutor::new(),
                    );
                    // WebSockets over HTTP/2 (RFC 8441).
                    builder.http2().enable_connect_protocol();
                    if let Err(e) = builder
                        .serve_connection_with_upgrades(hyper_util::rt::TokioIo::new(stream), svc)
                        .await
                    {
                        tracing::debug!(error = %e, "hub: connection ended");
                    }
                });
            }
        });
        let url = format!("wss://{addr}/hub");
        let _ = self.url.set(url.clone());
        let _ = self.fingerprint.set(identity.fingerprint());
        let _ = self.http3.set(h3);
        Ok(url)
    }

    /// HTTP/3 on QUIC (UDP) at `addr`, for files.
    fn serve_h3(
        self: &Arc<Self>,
        addr: SocketAddr,
        identity: &crate::hub_tls::Identity,
        token: Arc<String>,
    ) -> anyhow::Result<()> {
        // No 0-RTT: early data can be replayed.
        let tls = identity.server(&[crate::hub_tls::H3])?;
        let crypto = quinn::crypto::rustls::QuicServerConfig::try_from(tls)?;
        let endpoint =
            quinn::Endpoint::server(quinn::ServerConfig::with_crypto(Arc::new(crypto)), addr)?;
        let hub = self.clone();
        tokio::spawn(async move {
            while let Some(incoming) = endpoint.accept().await {
                let (hub, token) = (hub.clone(), token.clone());
                tokio::spawn(async move {
                    if let Err(e) = h3_connection(hub, incoming, token).await {
                        tracing::debug!(error = %e, "hub: HTTP/3 connection ended");
                    }
                });
            }
        });
        Ok(())
    }

    /// For the Hub page.
    pub fn status(&self) -> anyhow::Result<HubStatusView> {
        let st = self
            .hub
            .lock()
            .unwrap()
            .status(Utc::now())
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let (cpu, mem, cpus) = self.resources();
        let files = self
            .files
            .lock()
            .unwrap()
            .stats()
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        Ok(HubStatusView {
            runs_on: st.runs_on,
            protocol_version: st.protocol_version,
            started_at: time(st.started_at),
            url: self.url.get().cloned(),
            fingerprint: self.fingerprint.get().cloned(),
            http3: self.http3.get().copied().unwrap_or(false),
            path: self.path.display().to_string(),
            head: st.head,
            records: st.records,
            edges: st.edges,
            live_leases: st.live_leases,
            storage_bytes: st.storage_bytes,
            files: files.files,
            file_bytes: files.bytes,
            connections: st
                .connections
                .into_iter()
                .map(|c| ConnectionView {
                    this_app: c.id == APP,
                    id: c.id,
                    device: c.device,
                    client: c.client,
                    address: c.address,
                    connected_at: time(c.connected_at),
                    behind: st.head.saturating_sub(c.synced_to),
                    synced_to: c.synced_to,
                })
                .collect(),
            cpu_percent: cpu,
            memory_bytes: mem,
            cpus,
            pending: self
                .local
                .get()
                .and_then(|(s, _)| s.hub_outbox_len().ok())
                .unwrap_or(0),
            cloud_hub: self.cloud_link().map(|l| l.hub_id.clone()),
            cloud_connected: self.cloud_link().is_some_and(|l| l.is_connected()),
        })
    }

    /// This process's CPU (since the last call) and memory. The hub runs
    /// inside the app, so these are the app's numbers.
    fn resources(&self) -> (Option<f64>, Option<u64>, usize) {
        use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate};
        let Ok(pid) = sysinfo::get_current_pid() else {
            return (None, None, 0);
        };
        let cpus = std::thread::available_parallelism().map_or(1, |n| n.get());
        let mut guard = self.sys.lock().unwrap();
        let Some(sys) = guard.as_mut() else {
            return (None, None, cpus);
        };
        sys.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[pid]),
            true,
            ProcessRefreshKind::nothing().with_cpu().with_memory(),
        );
        match sys.process(Pid::from(pid.as_u32() as usize)) {
            Some(p) => (Some(p.cpu_usage() as f64), Some(p.memory()), cpus),
            None => (None, None, cpus),
        }
    }
}

/// Files, and the live channel.
impl LocalHub {
    /// Lets other devices steer the sessions this app runs.
    /// Also gives them somewhere to put the files their replies name.
    pub fn attach_agents(self: &Arc<Self>, agents: Arc<AgentHub>) {
        let hub = Arc::downgrade(self);
        agents.set_file_sink(Arc::new(move |bytes: &[u8]| {
            let hub = hub
                .upgrade()
                .ok_or_else(|| anyhow::anyhow!("the hub has shut down"))?;
            Ok(hub.file_put(bytes)?.hash)
        }));
        let _ = self.agents.set(agents);
    }

    /// Stores a file in the hub. The hub keeps it while a record mentions
    /// its hash (and a day after, for one that's on its way).
    pub fn file_put(&self, bytes: &[u8]) -> anyhow::Result<BlobMeta> {
        self.files
            .lock()
            .unwrap()
            .put(bytes, Utc::now())
            .map_err(|e| anyhow::anyhow!("{e}"))
    }

    pub fn file_get(&self, hash: &str) -> anyhow::Result<Option<Vec<u8>>> {
        self.files
            .lock()
            .unwrap()
            .get(hash)
            .map_err(|e| anyhow::anyhow!("{e}"))
    }

    /// Puts a hub file in the attachments folder `dir` as
    /// `<dir>/<hash hex>/<name>` and returns its path. Already there: just the path.
    pub fn file_materialize(&self, dir: &Path, hash: &str, name: &str) -> anyhow::Result<PathBuf> {
        let dest = attachment_path(dir, hash, name)
            .ok_or_else(|| anyhow::anyhow!("not a file hash: {hash}"))?;
        if dest.is_file() {
            return Ok(dest);
        }
        let here = |hub: &Self| hub.files.lock().unwrap().path(hash).filter(|p| p.is_file());
        let src = match here(self) {
            Some(p) => p,
            None => {
                self.fetch_from_cloud(hash)?;
                here(self).ok_or_else(|| anyhow::anyhow!("the hub doesn't have that file"))?
            }
        };
        std::fs::create_dir_all(dest.parent().expect("has a folder"))?;
        // A copy, not a link: an agent editing the attachment mustn't change
        // the bytes behind the hash. (On APFS and Btrfs this is a cheap clone.)
        std::fs::copy(&src, &dest)?;
        Ok(dest)
    }

    /// Copies a file from the cloud hub into this one. Blocks on the
    /// download. A file the cloud didn't have isn't asked for again for a
    /// minute (the chat polls the files it shows).
    fn fetch_from_cloud(&self, hash: &str) -> anyhow::Result<()> {
        let link = self
            .cloud_link()
            .ok_or_else(|| anyhow::anyhow!("the hub doesn't have that file"))?;
        if self
            .misses
            .lock()
            .unwrap()
            .get(hash)
            .is_some_and(|at| at.elapsed() < MISS_FOR)
        {
            anyhow::bail!("the hub doesn't have that file");
        }
        match link.get_file(hash) {
            Ok(Some(bytes)) => {
                self.file_put(&bytes)?;
                self.uploaded.lock().unwrap().insert(hash.to_string());
                Ok(())
            }
            Ok(None) => {
                self.misses
                    .lock()
                    .unwrap()
                    .insert(hash.to_string(), std::time::Instant::now());
                anyhow::bail!("the hub doesn't have that file")
            }
            Err(e) => {
                self.misses
                    .lock()
                    .unwrap()
                    .insert(hash.to_string(), std::time::Instant::now());
                Err(e)
            }
        }
    }

    /// Saves a file the person attached: into the hub, then into the
    /// attachments folder `dir` under its hash. Returns its path there.
    pub fn attach(&self, dir: &Path, bytes: &[u8], name: &str) -> anyhow::Result<PathBuf> {
        let meta = self.file_put(bytes)?;
        self.file_materialize(dir, &meta.hash, name)
    }

    /// Deletes files nothing refers to any more (after the grace period).
    pub fn collect_files(&self) -> anyhow::Result<usize> {
        // Synced with the cloud, this hub's records stop moving, so its
        // files are a cache of the cloud's; keep any that haven't gone up.
        if self.cloud_link().is_some() && !self.to_upload.lock().unwrap().is_empty() {
            return Ok(0);
        }
        let hub = self.hub.lock().unwrap();
        let mut files = self.files.lock().unwrap();
        let gone =
            tab_hub::blobs::collect(hub.store(), &mut *files, Utc::now(), tab_hub::blobs::GRACE)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
        Ok(gone.len())
    }

    /// Sends a command to the device running `session`. Returns how many of
    /// its connections got it; fails if it isn't connected.
    pub fn steer_remote(&self, session: &str, cmd: &SteerCommand) -> anyhow::Result<u32> {
        match self.request(Req::Steer(pb::Steer {
            session: session.into(),
            body_json: serde_json::to_vec(cmd)?,
        }))? {
            Res::Delivered(d) => Ok(d.connections),
            _ => anyhow::bail!("hub: unexpected reply to Steer"),
        }
    }

    /// The session of an approval prompt another device's session raised.
    pub fn remote_approval(&self, id: &str) -> Option<String> {
        self.remote_approvals.lock().unwrap().get(id).cloned()
    }

    /// Publishes a live event for a session this app runs, if anyone watches.
    fn publish(&self, event: &LiveEvent) {
        if self
            .hub
            .lock()
            .unwrap()
            .watchers(event.session())
            .is_empty()
        {
            return;
        }
        let Ok(json) = serde_json::to_vec(event) else {
            return;
        };
        if let Err(e) = self.request(Req::Publish(pb::Publish {
            session: event.session().into(),
            event_json: json,
        })) {
            tracing::debug!(error = %e, "hub: couldn't publish a live event");
        }
    }

    /// Watches the sessions other devices run that are mid-turn, so their
    /// streamed text and prompts show here.
    fn rewatch(&self, store: &Store) {
        let me = store.device_id().ok();
        let want: std::collections::BTreeSet<String> = store
            .sessions_recent(200)
            .unwrap_or_default()
            .into_iter()
            .filter(|s| s.device.is_some() && s.device != me)
            .filter(|s| matches!(s.status, SessionStatus::Working | SessionStatus::NeedsInput))
            .map(|s| s.id)
            .collect();
        let mut watching = self.watching.lock().unwrap();
        if *watching == want {
            return;
        }
        *watching = want.clone();
        drop(watching);
        if let Err(e) = self.request(Req::Watch(pb::Watch {
            sessions: want.into_iter().collect(),
        })) {
            tracing::debug!(error = %e, "hub: couldn't update what this app watches");
        }
    }

    /// The live side of [`Self::follow`]: publishes this app's streamed
    /// events for watchers, handles steering and watch notices addressed to
    /// it, and shows other devices' live events here.
    fn run_live(self: &Arc<Self>, store: Arc<Store>, events: broadcast::Sender<EngineEvent>) {
        // Text streamed so far in each session's current reply, for
        // watchers who join mid-turn.
        let partial: Arc<Mutex<std::collections::HashMap<String, String>>> = Default::default();

        let (me, mut rx, part, st) = (
            self.clone(),
            events.subscribe(),
            partial.clone(),
            store.clone(),
        );
        tokio::spawn(async move {
            loop {
                let e = match rx.recv().await {
                    Ok(e) => e,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                };
                match &e {
                    EngineEvent::SessionDelta { session_id, text } => {
                        part.lock()
                            .unwrap()
                            .entry(session_id.clone())
                            .or_default()
                            .push_str(text);
                    }
                    EngineEvent::SessionMessage { message } => {
                        part.lock().unwrap().remove(&message.session_id);
                    }
                    EngineEvent::SessionChanged { session } => {
                        if session.status != SessionStatus::Working {
                            part.lock().unwrap().remove(&session.id);
                        }
                        me.rewatch(&st);
                    }
                    _ => {}
                }
                let Some(live) = LiveEvent::from_engine(&e) else {
                    continue;
                };
                // Only sessions this app runs, and only while someone watches.
                if me.hub.lock().unwrap().watchers(live.session()).is_empty() {
                    continue;
                }
                let mine = me.agents.get().is_some_and(|a| {
                    st.session_get(live.session())
                        .ok()
                        .flatten()
                        .is_some_and(|s| a.runs_here(&s))
                });
                if mine {
                    me.publish(&live);
                }
            }
        });

        let Some(mut inbox) = self.inbox_rx.lock().unwrap().take() else {
            return;
        };
        let me = self.clone();
        tokio::spawn(async move {
            me.rewatch(&store);
            while let Some(env) = inbox.recv().await {
                match env.body {
                    Some(Res::Steered(s)) => me.steered(&store, s).await,
                    Some(Res::Watched(w)) if w.watchers > 0 => {
                        let text = partial.lock().unwrap().get(&w.session).cloned();
                        if let Some(text) = text.filter(|t| !t.is_empty()) {
                            me.publish(&LiveEvent::SessionDelta {
                                session_id: w.session.clone(),
                                text,
                            });
                        }
                        if let Some(agents) = me.agents.get() {
                            for approval in agents.pending_approvals(&w.session) {
                                me.publish(&LiveEvent::ApprovalRequested { approval });
                            }
                        }
                    }
                    Some(Res::Live(l)) => {
                        let Ok(live) = serde_json::from_slice::<LiveEvent>(&l.event_json) else {
                            continue;
                        };
                        if live.session() != l.session {
                            continue;
                        }
                        let mut remote = me.remote_approvals.lock().unwrap();
                        match &live {
                            LiveEvent::ApprovalRequested { approval } => {
                                remote.insert(approval.id.clone(), approval.session_id.clone());
                            }
                            LiveEvent::ApprovalResolved { id, .. } => {
                                remote.remove(id);
                            }
                            _ => {}
                        }
                        drop(remote);
                        if let Some((_, events)) = me.local.get() {
                            let _ = events.send(live.into_engine());
                        }
                    }
                    _ => {}
                }
            }
        });

        let me = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(COLLECT).await;
                match me.collect_files() {
                    Ok(0) => {}
                    Ok(n) => tracing::info!(files = n, "hub: removed files nothing refers to"),
                    Err(e) => tracing::warn!(error = %e, "hub: cleaning up files failed"),
                }
            }
        });
    }

    /// Another device steering a session this app runs.
    async fn steered(&self, store: &Store, s: pb::Steered) {
        let Some(agents) = self.agents.get() else {
            return;
        };
        let session = match store.session_get(&s.session) {
            Ok(Some(session)) if agents.runs_here(&session) => session,
            _ => return,
        };
        let cmd = match serde_json::from_slice::<SteerCommand>(&s.body_json) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, from = %s.from, "hub: unreadable steering");
                return;
            }
        };
        tracing::info!(session = %session.id, from = %s.from, "hub: steered from another device");
        let result = match cmd {
            SteerCommand::Message { text, files } => {
                self.steer_message(agents, &session.id, &text, &files)
            }
            SteerCommand::Approval {
                id,
                decision,
                message,
                answers,
            } => {
                agents.resolve_approval(&id, Decision::from_parts(&decision, message, answers));
                Ok(())
            }
            SteerCommand::Interrupt => {
                agents.interrupt(&session.id);
                Ok(())
            }
            SteerCommand::Stop => {
                agents.stop(&session.id);
                Ok(())
            }
        };
        if let Err(e) = result {
            tracing::warn!(session = %session.id, error = %e, "hub: steering failed");
        }
    }

    fn steer_message(
        &self,
        agents: &Arc<AgentHub>,
        session: &str,
        text: &str,
        files: &[FileRef],
    ) -> anyhow::Result<()> {
        let dir = agents.attachments_dir();
        let paths = files
            .iter()
            .map(|f| {
                self.file_materialize(&dir, &f.hash, &f.name)
                    .map(|p| p.display().to_string())
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let text = if text.trim().is_empty() && !paths.is_empty() {
            "Have a look at this."
        } else {
            text
        };
        if !agents.steer_with(session, text, &paths)? {
            agents.send_with(session, text, &paths)?;
        }
        Ok(())
    }
}

fn reply_ok(h: Handled) -> anyhow::Result<()> {
    match h.reply.body {
        Some(pb::server_envelope::Body::Error(e)) => Err(anyhow::anyhow!("hub: {}", e.message)),
        _ => Ok(()),
    }
}

const WS_PROTOCOL: &str = "tab.hub";
const WS_TOKEN: &str = "tab.token.";

/// Whether a request carries the token: `Authorization: Bearer <token>`,
/// or a WebSocket subprotocol `tab.token.<token>` (browsers can't set
/// headers on a WebSocket).
fn authorized(headers: &axum::http::HeaderMap, token: &str) -> bool {
    if token.is_empty() {
        return false;
    }
    let bearer = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    let proto = headers
        .get_all(axum::http::header::SEC_WEBSOCKET_PROTOCOL)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .find_map(|p| p.trim().strip_prefix(WS_TOKEN));
    bearer
        .into_iter()
        .chain(proto)
        .any(|t| crate::hub_tls::same_secret(t, token))
}

fn unauthorized() -> Response {
    (StatusCode::UNAUTHORIZED, "missing or wrong token").into_response()
}

async fn upgrade(
    State((hub, token)): State<(Arc<LocalHub>, Arc<String>)>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: axum::http::HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    if !authorized(&headers, &token) {
        return unauthorized();
    }
    ws.protocols([WS_PROTOCOL])
        .on_upgrade(move |socket| connection(hub, socket, addr))
}

/// `PUT /files` with the file's bytes: stores it and answers its hash and
/// size as JSON.
async fn file_upload(
    State((hub, token)): State<(Arc<LocalHub>, Arc<String>)>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if !authorized(&headers, &token) {
        return unauthorized();
    }
    match hub.file_put(&body) {
        Ok(m) => axum::Json(serde_json::json!({ "hash": m.hash, "size": m.size })).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    }
}

/// `GET /files/<hash>`: the file's bytes.
async fn file_download(
    State((hub, token)): State<(Arc<LocalHub>, Arc<String>)>,
    axum::extract::Path(hash): axum::extract::Path<String>,
    headers: axum::http::HeaderMap,
) -> Response {
    if !authorized(&headers, &token) {
        return unauthorized();
    }
    match hub.file_get(&hash) {
        Ok(Some(bytes)) => (
            [
                (axum::http::header::CONTENT_TYPE, "application/octet-stream"),
                // Content-addressed: the bytes behind a hash never change.
                (
                    axum::http::header::CACHE_CONTROL,
                    "private, max-age=31536000, immutable",
                ),
            ],
            bytes,
        )
            .into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, "no such file").into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// One HTTP/3 connection: the same file routes as over TCP.
async fn h3_connection(
    hub: Arc<LocalHub>,
    incoming: quinn::Incoming,
    token: Arc<String>,
) -> anyhow::Result<()> {
    let conn = incoming.await?;
    let mut h3c: h3::server::Connection<h3_quinn::Connection, bytes::Bytes> = h3::server::builder()
        .build(h3_quinn::Connection::new(conn))
        .await?;
    while let Some(resolver) = h3c.accept().await? {
        let (req, stream) = resolver.resolve_request().await?;
        let (hub, token) = (hub.clone(), token.clone());
        tokio::spawn(async move {
            if let Err(e) = h3_request(hub, req, stream, token).await {
                tracing::debug!(error = %e, "hub: HTTP/3 request failed");
            }
        });
    }
    Ok(())
}

async fn h3_request(
    hub: Arc<LocalHub>,
    req: http::Request<()>,
    mut stream: h3::server::RequestStream<h3_quinn::BidiStream<bytes::Bytes>, bytes::Bytes>,
    token: Arc<String>,
) -> anyhow::Result<()> {
    use bytes::Buf;
    let reply = |status: StatusCode, kind: &'static str, body: Vec<u8>| (status, kind, body);
    let (status, kind, body) = if !authorized(req.headers(), &token) {
        reply(
            StatusCode::UNAUTHORIZED,
            "text/plain",
            b"missing or wrong token".to_vec(),
        )
    } else {
        match (req.method().as_str(), req.uri().path()) {
            ("GET", path) if path.starts_with("/files/") => {
                match hub.file_get(&path["/files/".len()..]) {
                    Ok(Some(b)) => reply(StatusCode::OK, "application/octet-stream", b),
                    Ok(None) => reply(
                        StatusCode::NOT_FOUND,
                        "text/plain",
                        b"no such file".to_vec(),
                    ),
                    Err(e) => reply(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "text/plain",
                        e.to_string().into_bytes(),
                    ),
                }
            }
            ("PUT", "/files") => {
                let mut buf = Vec::new();
                let mut too_big = false;
                while let Some(mut chunk) = stream.recv_data().await? {
                    if buf.len() + chunk.remaining() > tab_domain::blob::MAX_BLOB_BYTES {
                        too_big = true;
                        break;
                    }
                    buf.extend_from_slice(&chunk.copy_to_bytes(chunk.remaining()));
                }
                if too_big {
                    reply(
                        StatusCode::PAYLOAD_TOO_LARGE,
                        "text/plain",
                        b"file too large".to_vec(),
                    )
                } else {
                    match hub.file_put(&buf) {
                        Ok(m) => reply(
                            StatusCode::OK,
                            "application/json",
                            serde_json::to_vec(
                                &serde_json::json!({ "hash": m.hash, "size": m.size }),
                            )?,
                        ),
                        Err(e) => reply(
                            StatusCode::BAD_REQUEST,
                            "text/plain",
                            e.to_string().into_bytes(),
                        ),
                    }
                }
            }
            _ => reply(StatusCode::NOT_FOUND, "text/plain", b"not here".to_vec()),
        }
    };
    let mut res = http::Response::builder()
        .status(status)
        .header(http::header::CONTENT_TYPE, kind);
    if status == StatusCode::OK && kind == "application/octet-stream" {
        res = res.header(
            http::header::CACHE_CONTROL,
            "private, max-age=31536000, immutable",
        );
    }
    stream.send_response(res.body(())?).await?;
    stream.send_data(bytes::Bytes::from(body)).await?;
    stream.finish().await?;
    Ok(())
}

async fn connection(hub: Arc<LocalHub>, mut socket: WebSocket, addr: SocketAddr) {
    let conn = hub.next_conn.fetch_add(1, Ordering::Relaxed);
    let mut pushes = hub.pushes.subscribe();
    hub.hub
        .lock()
        .unwrap()
        .connect(conn, &addr.to_string(), Utc::now());
    tracing::info!(conn, %addr, "hub: device connected");
    loop {
        tokio::select! {
            msg = socket.recv() => {
                let frame = match msg {
                    Some(Ok(Message::Binary(b))) => b,
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    Some(Ok(_)) => continue,
                };
                let handled = hub.hub.lock().unwrap().handle_frame(conn, &frame, Utc::now());
                if handled.push.is_some() {
                    hub.remote.notify_one();
                }
                hub.deliver(conn, handled.push.as_ref(), handled.direct);
                if socket.send(Message::Binary(encode(&handled.reply).into())).await.is_err() {
                    break;
                }
            }
            push = pushes.recv() => {
                match push {
                    Ok((to, frame)) if to == To::Only(conn) || matches!(to, To::AllBut(from) if from != conn) => {
                        let subscribed = hub.hub.lock().unwrap().subscribers().any(|c| c == conn);
                        if subscribed && socket.send(Message::Binary(frame.to_vec().into())).await.is_err() {
                            break;
                        }
                    }
                    Ok(_) => {}
                    // Too far behind for pushes: it'll catch up by asking.
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
    let told = hub.hub.lock().unwrap().disconnect(conn);
    hub.deliver(conn, None, told);
    tracing::info!(conn, "hub: device disconnected");
}

#[cfg(test)]
#[path = "local_hub_tests.rs"]
mod tests;

/// Leases on the hub `tab.db` syncs with: this app's own, or the cloud hub
/// once moved, where every runner (each computer, and tab cloud) claims.
impl crate::leases::Leases for LocalHub {
    /// The device id this app says hello with (to its own hub once
    /// following, and to the cloud hub), which the hub names as holder.
    fn holder(&self) -> String {
        self.local
            .get()
            .and_then(|(store, _)| store.device_id().ok())
            .unwrap_or_else(|| "this-computer".into())
    }

    fn claim_blocking(
        &self,
        key: &str,
        ttl: chrono::Duration,
    ) -> anyhow::Result<crate::leases::Claimed> {
        let ttl_secs = ttl.num_seconds().clamp(1, u32::MAX as i64) as u32;
        let Res::LeaseClaim(c) = self.up(Req::ClaimLease(pb::ClaimLease {
            key: key.into(),
            ttl_secs,
        }))?
        else {
            anyhow::bail!("hub: unexpected reply to ClaimLease");
        };
        let claim = tab_domain::lease::Claim::try_from(c)
            .map_err(|e| anyhow::anyhow!("hub: bad lease reply: {e:?}"))?;
        let lease = claim.lease();
        Ok(crate::leases::Claimed {
            won: claim.won(),
            token: lease.token,
            holder: lease.holder.clone(),
            expires_at: lease.expires_at,
        })
    }

    fn release_blocking(&self, key: &str) -> anyhow::Result<()> {
        self.up(Req::ReleaseLease(pb::ReleaseLease { key: key.into() }))
            .map(|_| ())
    }
}
