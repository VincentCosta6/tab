//! The hub, running inside the desktop app.
//!
//! This is the "free" setup from the plan: [`tab_hub::Hub`] on a local
//! SQLite file (`hub.db`, next to `tab.db`). The hub's change log is the
//! record every device syncs from, and `tab.db` is this device's copy (see
//! [`crate::store::hub_sync`]): every change to a synced table is queued in
//! the same transaction and sent to the hub moments later as a whole-row
//! `Put` or `Delete`, and changes other devices send are written back into
//! `tab.db` and announced to the UI. The first launch copies everything
//! already in `tab.db` into the hub.
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
use tab_protocol::pb::{self, client_envelope::Body as Req};
use tab_protocol::{ServerEnvelope, encode};
use tab_store::HubStore;
use tab_store::sqlite::SqliteStore;
use tokio::sync::broadcast;

use crate::engine::{Change, EngineEvent};
use crate::store::Store;

/// The desktop app's own connection to its hub.
const APP: ConnId = 0;
/// How often lapsed leases are expired.
const TICK: Duration = Duration::from_secs(5);
/// How often the outbox is checked when it was empty.
const DRAIN: Duration = Duration::from_millis(150);

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
        }))
    }

    /// Runs one request as the app, sending any push to other devices.
    fn request(&self, req: Req) -> anyhow::Result<()> {
        let id = self.next_request.fetch_add(1, Ordering::Relaxed);
        let handled = self.hub.lock().unwrap().handle(APP, id, req, Utc::now());
        if let Some(push) = &handled.push {
            let _ = self.pushes.send((APP, Arc::new(encode(push))));
        }
        reply_ok(handled)
    }

    /// Sends one batch from the outbox. Returns how many rows it handled.
    fn drain_once(&self, store: &Store) -> anyhow::Result<usize> {
        let rows = store.hub_outbox_take(tab_domain::ops::MAX_BATCH)?;
        let Some(last) = rows.last().map(|r| r.seq) else {
            return Ok(0);
        };
        let mut ops = Vec::with_capacity(rows.len());
        {
            let hub = self.hub.lock().unwrap();
            let mut seen = std::collections::HashSet::new();
            for r in rows.iter().rev() {
                // The same row queued twice carries the same contents: send it once.
                if !seen.insert((r.table.as_str(), r.key.as_str())) {
                    continue;
                }
                let stored = hub
                    .store()
                    .get(&r.table, &r.key)
                    .map_err(|e| anyhow::anyhow!("{e}"))?
                    .map(|rec| rec.body);
                match (&r.row, stored) {
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
        }
        ops.reverse();
        if !ops.is_empty() {
            self.request(Req::Apply(pb::Apply {
                ops: ops.iter().map(Into::into).collect(),
            }))?;
        }
        store.hub_outbox_done(last)?;
        Ok(rows.len())
    }

    /// Starts syncing `tab.db` with the hub: copies what's there the first
    /// time, sends local changes as they happen, and expires leases.
    pub fn follow(self: &Arc<Self>, store: Arc<Store>, events: broadcast::Sender<EngineEvent>) {
        let _ = self.local.set((store.clone(), events));
        let me = self.clone();
        tokio::spawn(async move {
            let setup = store
                .hub_sync_install()
                .and_then(|_| store.hub_backfill_once());
            match setup {
                Ok(0) => {}
                Ok(n) => tracing::info!(rows = n, "hub: copying tab.db into the hub"),
                Err(e) => {
                    tracing::error!(error = %e, "hub: couldn't set up sync; staying local");
                    return;
                }
            }
            loop {
                match me.drain_once(&store) {
                    Ok(n) if n > 0 => continue,
                    Ok(_) => {}
                    Err(e) => tracing::warn!(error = %e, "hub: sending changes failed; will retry"),
                }
                tokio::time::sleep(DRAIN).await;
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

    /// Writes changes another device made into `tab.db` and tells the UI.
    fn apply_remote(&self, push: &ServerEnvelope) {
        let Some(pb::server_envelope::Body::Changes(c)) = &push.body else {
            return;
        };
        let Some((store, events)) = self.local.get() else {
            return;
        };
        let mut rows = vec![];
        for a in &c.changes {
            let Some(op) = a.op.clone().and_then(|o| Op::try_from(o).ok()) else {
                continue;
            };
            match op {
                Op::Put { kind, id, body } => rows.push((kind, id, Some(body))),
                Op::Delete { kind, id } => rows.push((kind, id, None)),
                _ => {}
            }
        }
        if let Err(e) = store.hub_apply_remote(&rows) {
            tracing::warn!(error = %e, "hub: couldn't apply another device's changes");
            return;
        }
        for (table, key, _) in &rows {
            if table != "items" {
                continue;
            }
            let id: Option<String> = serde_json::from_str::<Vec<String>>(key)
                .ok()
                .and_then(|k| k.into_iter().next());
            if let Some(item) = id.and_then(|id| store.get(&id).ok().flatten()) {
                let _ = events.send(EngineEvent::ItemChanged {
                    change: Change::Updated,
                    item,
                });
            }
        }
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
                    hub.apply_remote(push);
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
