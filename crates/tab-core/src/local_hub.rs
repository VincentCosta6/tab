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
//! loop can later point at a hub in the cloud.
//!
//! The hub also listens for other devices on a localhost WebSocket
//! (`ws://127.0.0.1:<port>/hub?token=…`, binary Protobuf frames from
//! `tab-protocol`), and reports what it's doing for the Hub page in
//! Settings ([`LocalHub::status`]).

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use axum::Router;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use chrono::{DateTime, Utc};
use serde::Serialize;
use tab_domain::ops::Op;
use tab_hub::{ConnId, Handled, Hub};
use tab_protocol::encode;
use tab_protocol::pb::{self, client_envelope::Body as Req};
use tab_store::HubStore;
use tab_store::sqlite::SqliteStore;
use tokio::sync::broadcast;

use crate::engine::{Change, EngineEvent};
use crate::store::Store;

/// The desktop app's own connection to its hub.
const APP: ConnId = 0;
/// How often lapsed leases are expired.
const TICK: Duration = Duration::from_secs(5);
/// How often sync runs when nothing woke it, as a backstop.
const IDLE: Duration = Duration::from_secs(5);
/// Changes read from the hub per request.
const PULL: u32 = 500;

pub struct LocalHub {
    hub: Mutex<Hub<SqliteStore>>,
    path: PathBuf,
    /// Frames to push, with the connection that caused them.
    pushes: broadcast::Sender<(ConnId, Arc<Vec<u8>>)>,
    next_conn: AtomicU64,
    next_request: AtomicU64,
    url: OnceLock<String>,
    /// This device's copy and the engine's event stream, once following.
    local: OnceLock<(Arc<Store>, broadcast::Sender<EngineEvent>)>,
    /// None where the OS won't say.
    sys: Mutex<Option<sysinfo::System>>,
    /// Woken when another device changes something.
    remote: tokio::sync::Notify,
}

/// What the Hub page in Settings shows.
#[derive(Debug, Clone, Serialize)]
pub struct HubStatusView {
    pub runs_on: String,
    pub protocol_version: u32,
    pub started_at: Option<DateTime<Utc>>,
    /// Where other devices connect, once the listener is up.
    pub url: Option<String>,
    pub path: String,
    pub head: u64,
    pub records: u64,
    pub edges: u64,
    pub live_leases: u64,
    pub storage_bytes: u64,
    pub connections: Vec<ConnectionView>,
    pub cpu_percent: Option<f64>,
    pub memory_bytes: Option<u64>,
    pub cpus: usize,
    /// Local changes not yet sent to the hub.
    pub pending: u64,
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
        Ok(Arc::new(Self {
            hub: Mutex::new(hub),
            path: path.to_path_buf(),
            pushes,
            next_conn: AtomicU64::new(1),
            next_request: AtomicU64::new(1),
            url: OnceLock::new(),
            local: OnceLock::new(),
            sys: Mutex::new(sysinfo::System::new().ok()),
            remote: tokio::sync::Notify::new(),
        }))
    }

    /// Runs one request as the app, sending any push to other devices.
    fn request(&self, req: Req) -> anyhow::Result<pb::server_envelope::Body> {
        let id = self.next_request.fetch_add(1, Ordering::Relaxed);
        let handled = self.hub.lock().unwrap().handle(APP, id, req, Utc::now());
        if let Some(push) = &handled.push {
            let _ = self.pushes.send((APP, Arc::new(encode(push))));
        }
        match handled.reply.body {
            Some(pb::server_envelope::Body::Error(e)) => Err(anyhow::anyhow!("hub: {}", e.message)),
            Some(body) => Ok(body),
            None => Err(anyhow::anyhow!("hub: empty reply")),
        }
    }

    fn apply(&self, ops: &[Op]) -> anyhow::Result<()> {
        self.request(Req::Apply(pb::Apply {
            ops: ops.iter().map(Into::into).collect(),
        }))
        .map(|_| ())
    }

    fn hub_record(&self, kind: &str, id: &str) -> anyhow::Result<Option<serde_json::Value>> {
        Ok(self
            .hub
            .lock()
            .unwrap()
            .store()
            .get(kind, id)
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .map(|r| r.body))
    }

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
        for r in rows.iter().rev() {
            // The same row queued twice carries the same contents: send it once.
            if !seen.insert((r.table.as_str(), r.key.as_str())) {
                continue;
            }
            match (&r.row, self.hub_record(&r.table, &r.key)?) {
                (Some(row), Some(old)) if *row == old => {}
                (None, None) => {}
                (Some(row), _) => ops.push(Op::Put {
                    kind: r.table.clone(),
                    id: r.key.clone(),
                    body: row.clone(),
                }),
                (None, Some(_)) => ops.push(Op::Delete {
                    kind: r.table.clone(),
                    id: r.key.clone(),
                }),
            }
        }
        ops.reverse();
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

    /// Reads one page of the hub's log into `tab.db`. Returns whether
    /// there's more.
    fn pull_once(&self, store: &Store) -> anyhow::Result<bool> {
        let since = store.hub_pulled_to()?;
        let pb::server_envelope::Body::Changes(page) =
            self.request(Req::GetChanges(pb::GetChanges { since, limit: PULL }))?
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
        let head = self.head()?;
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
        let _ = self.local.set((store.clone(), events));
        let me = self.clone();
        tokio::spawn(async move {
            if let Err(e) = me.introduce(&store) {
                tracing::error!(error = %e, "hub: couldn't set up sync; staying local");
                return;
            }
            let commits = store.commits();
            loop {
                if let Err(e) = me.sync(&store) {
                    tracing::warn!(error = %e, "hub: sync failed; will retry");
                }
                tokio::select! {
                    _ = commits.notified() => {}
                    _ = me.remote.notified() => {}
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
                    Ok(Some(p)) => {
                        let _ = me.pushes.send((APP, Arc::new(encode(&p))));
                    }
                    Ok(None) => {}
                    Err(e) => tracing::warn!(error = %e, "hub: expiring leases failed"),
                }
            }
        });
    }

    /// Listens for other devices on localhost. Returns the URL (without
    /// the token).
    pub async fn serve(self: &Arc<Self>, port: u16, token: String) -> anyhow::Result<String> {
        let mut listener = None;
        for p in port..port.saturating_add(20) {
            if let Ok(l) = tokio::net::TcpListener::bind(("127.0.0.1", p)).await {
                listener = Some(l);
                break;
            }
        }
        let listener = listener.ok_or_else(|| anyhow::anyhow!("no free port for the hub"))?;
        let url = format!("ws://{}/hub", listener.local_addr()?);
        let app = Router::new()
            .route("/hub", get(upgrade))
            .with_state((self.clone(), Arc::new(token)));
        tokio::spawn(async move {
            let svc = app.into_make_service_with_connect_info::<SocketAddr>();
            if let Err(e) = axum::serve(listener, svc).await {
                tracing::error!(error = %e, "hub listener stopped");
            }
        });
        let _ = self.url.set(url.clone());
        Ok(url)
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
        Ok(HubStatusView {
            runs_on: st.runs_on,
            protocol_version: st.protocol_version,
            started_at: time(st.started_at),
            url: self.url.get().cloned(),
            path: self.path.display().to_string(),
            head: st.head,
            records: st.records,
            edges: st.edges,
            live_leases: st.live_leases,
            storage_bytes: st.storage_bytes,
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

fn reply_ok(h: Handled) -> anyhow::Result<()> {
    match h.reply.body {
        Some(pb::server_envelope::Body::Error(e)) => Err(anyhow::anyhow!("hub: {}", e.message)),
        _ => Ok(()),
    }
}

#[derive(serde::Deserialize)]
struct Auth {
    #[serde(default)]
    token: String,
}

async fn upgrade(
    State((hub, token)): State<(Arc<LocalHub>, Arc<String>)>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Query(auth): Query<Auth>,
    ws: WebSocketUpgrade,
) -> Response {
    if token.is_empty() || auth.token != *token {
        return (StatusCode::UNAUTHORIZED, "missing or wrong token").into_response();
    }
    ws.on_upgrade(move |socket| connection(hub, socket, addr))
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
                if let Some(push) = &handled.push {
                    hub.remote.notify_one();
                    let _ = hub.pushes.send((conn, Arc::new(encode(push))));
                }
                if socket.send(Message::Binary(encode(&handled.reply).into())).await.is_err() {
                    break;
                }
            }
            push = pushes.recv() => {
                match push {
                    Ok((from, frame)) if from != conn => {
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
    hub.hub.lock().unwrap().disconnect(conn);
    tracing::info!(conn, "hub: device disconnected");
}

#[cfg(test)]
#[path = "local_hub_tests.rs"]
mod tests;
