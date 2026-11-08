//! The app's connection to a hub in tab cloud.
//!
//! A cloud hub speaks the same protocol as the one in the desktop app
//! (hub.proto), over a WebSocket through the edge Worker:
//! `wss://api.tabqueue.com/v1/hubs/{hub_id}/sync`, with the signed-in
//! person's access token in `Authorization`. The edge checks the token and
//! membership; the hub checks each request against their role.
//!
//! [`CloudLink`] keeps one socket open, reconnecting with a fresh token when
//! it drops, and turns it into request/reply calls. The calls block (sync
//! runs on a blocking thread, like its SQLite work), so call them from
//! `spawn_blocking`, never on the async runtime. Pushes (other devices'
//! changes) wake [`CloudLink::changed`], so sync can read them straight away.
//! The live channel's pushes (steering, live events, watch notices) go to
//! the sink set with [`CloudLink::set_live`], the same inbox the app's own
//! hub feeds, so watching and steering work across computers on one cloud
//! hub as they do on a local one.
//! A `ws://` address (a local `wrangler dev`) works too, for dev builds.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::future::BoxFuture;
use futures_util::{SinkExt, StreamExt};
use tab_protocol::pb::{self, client_envelope::Body as Req, server_envelope::Body as Res};
use tab_protocol::{ClientEnvelope, ServerEnvelope, encode};
use tokio::sync::{Notify, mpsc};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

/// How long one request may take before it's given up on.
const TIMEOUT: Duration = Duration::from_secs(30);
/// Waits between reconnects, growing to the last.
const BACKOFF: [u64; 5] = [1, 2, 5, 10, 30];

/// Gets a current access token (refreshing it if it's about to expire).
pub type TokenSource = Arc<dyn Fn() -> BoxFuture<'static, anyhow::Result<String>> + Send + Sync>;

type Waiting = Arc<Mutex<HashMap<u64, std::sync::mpsc::SyncSender<Res>>>>;
/// Where the live channel's pushes go.
type LiveSink = Arc<Mutex<Option<mpsc::UnboundedSender<ServerEnvelope>>>>;

pub struct CloudLink {
    /// The hub's sync address.
    pub url: String,
    pub hub_id: String,
    outgoing: mpsc::UnboundedSender<Vec<u8>>,
    waiting: Waiting,
    next: AtomicU64,
    /// Woken when the hub pushes changes, and after each reconnect.
    pub changed: Arc<Notify>,
    /// Said again on every reconnect, so the hub knows who this is.
    hello: Arc<Mutex<Option<pb::Hello>>>,
    /// The sessions this app watches, said again after every reconnect
    /// (the hub forgets a connection's watches when it drops).
    watch: Arc<Mutex<Option<pb::Watch>>>,
    live: LiveSink,
    connected: Arc<std::sync::atomic::AtomicBool>,
}

/// `https://api.tabqueue.com` and a hub id to that hub's sync address.
pub fn sync_url(api: &str, hub_id: &str) -> String {
    let base = api.trim_end_matches('/');
    let ws = if let Some(rest) = base.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = base.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        base.to_string()
    };
    format!("{ws}/v1/hubs/{hub_id}/sync")
}

fn tls() -> tokio_tungstenite::Connector {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let cfg = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .expect("ring supports the default TLS versions")
    .with_root_certificates(roots)
    .with_no_client_auth();
    tokio_tungstenite::Connector::Rustls(Arc::new(cfg))
}

impl CloudLink {
    /// Starts connecting to `url` in the background. Requests wait for the
    /// socket (up to [`TIMEOUT`]).
    pub fn start(url: String, hub_id: String, token: TokenSource) -> Arc<Self> {
        let (outgoing, rx) = mpsc::unbounded_channel();
        let link = Arc::new(Self {
            url,
            hub_id,
            outgoing,
            waiting: Default::default(),
            next: AtomicU64::new(1),
            changed: Arc::new(Notify::new()),
            hello: Default::default(),
            watch: Default::default(),
            live: Default::default(),
            connected: Default::default(),
        });
        tokio::spawn(run(
            link.url.clone(),
            token,
            rx,
            Shared {
                waiting: link.waiting.clone(),
                changed: link.changed.clone(),
                hello: link.hello.clone(),
                watch: link.watch.clone(),
                live: link.live.clone(),
                connected: link.connected.clone(),
            },
        ));
        link
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    /// Says hello (and again after every reconnect). Returns the hub's head.
    pub fn hello(&self, hello: pb::Hello) -> anyhow::Result<u64> {
        *self.hello.lock().unwrap() = Some(hello.clone());
        match self.request(Req::Hello(hello))? {
            Res::Welcome(w) => Ok(w.head),
            other => anyhow::bail!("cloud hub: unexpected reply to hello: {other:?}"),
        }
    }

    /// Where steering, live events and watch notices from the hub go.
    pub fn set_live(&self, sink: mpsc::UnboundedSender<ServerEnvelope>) {
        *self.live.lock().unwrap() = Some(sink);
    }

    /// Watches `sessions` (replacing what was watched), now and after every
    /// reconnect. Doesn't wait for the reply.
    pub fn watch(&self, sessions: Vec<String>) {
        let w = pb::Watch { sessions };
        *self.watch.lock().unwrap() = Some(w.clone());
        self.send(Req::Watch(w));
    }

    /// Sends one request without waiting for its reply (a live event, which
    /// is no use late). Dropped while disconnected.
    pub fn send(&self, req: Req) {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let _ = self.outgoing.send(encode(&ClientEnvelope::new(id, req)));
    }

    /// Sends one request and waits for its reply. A hub error is an `Err`.
    pub fn request(&self, req: Req) -> anyhow::Result<Res> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        self.waiting.lock().unwrap().insert(id, tx);
        let frame = encode(&ClientEnvelope::new(id, req));
        if self.outgoing.send(frame).is_err() {
            self.waiting.lock().unwrap().remove(&id);
            anyhow::bail!("cloud hub: the connection has shut down");
        }
        let reply = rx.recv_timeout(TIMEOUT);
        self.waiting.lock().unwrap().remove(&id);
        use std::sync::mpsc::RecvTimeoutError;
        match reply {
            Ok(Res::Error(e)) => anyhow::bail!("cloud hub: {}", e.message),
            Ok(body) => Ok(body),
            Err(RecvTimeoutError::Disconnected) => {
                anyhow::bail!("cloud hub: not connected right now")
            }
            Err(RecvTimeoutError::Timeout) => {
                anyhow::bail!("cloud hub: no reply in {}s", TIMEOUT.as_secs())
            }
        }
    }
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn open(url: &str, token: &TokenSource) -> anyhow::Result<Socket> {
    let mut req = url.into_client_request()?;
    let token = token().await?;
    req.headers_mut()
        .insert("authorization", format!("Bearer {token}").parse()?);
    req.headers_mut().insert(
        "user-agent",
        format!("tab-desktop/{}", env!("CARGO_PKG_VERSION")).parse()?,
    );
    let (ws, _) =
        tokio_tungstenite::connect_async_tls_with_config(req, None, false, Some(tls())).await?;
    Ok(ws)
}

/// What the socket task shares with its [`CloudLink`].
struct Shared {
    waiting: Waiting,
    changed: Arc<Notify>,
    hello: Arc<Mutex<Option<pb::Hello>>>,
    watch: Arc<Mutex<Option<pb::Watch>>>,
    live: LiveSink,
    connected: Arc<std::sync::atomic::AtomicBool>,
}

/// What to do with a frame the hub sent unasked (request id 0).
#[derive(Debug, PartialEq)]
enum Push {
    /// Other devices changed something: sync.
    Changed,
    /// The live channel: for the app's inbox.
    Live,
    Ignore,
}

fn push_kind(body: &Option<Res>) -> Push {
    match body {
        Some(Res::Changes(_)) => Push::Changed,
        Some(Res::Steered(_) | Res::Live(_) | Res::Watched(_)) => Push::Live,
        _ => Push::Ignore,
    }
}

/// Keeps the socket up for as long as the link exists.
async fn run(
    url: String,
    token: TokenSource,
    mut outgoing: mpsc::UnboundedReceiver<Vec<u8>>,
    sh: Shared,
) {
    let Shared {
        waiting,
        changed,
        hello,
        watch,
        live,
        connected,
    } = sh;
    let mut attempt = 0usize;
    loop {
        let ws = match open(&url, &token).await {
            Ok(ws) => ws,
            Err(e) => {
                let wait = BACKOFF[attempt.min(BACKOFF.len() - 1)];
                tracing::warn!(error = %e, retry_in = wait, "cloud hub: couldn't connect");
                attempt += 1;
                // Requests made meanwhile fail rather than hang.
                fail_all(&waiting);
                if wait_or_closed(&mut outgoing, &waiting, Duration::from_secs(wait)).await {
                    return;
                }
                continue;
            }
        };
        attempt = 0;
        connected.store(true, Ordering::Relaxed);
        tracing::info!(%url, "cloud hub: connected");
        let (mut tx, mut rx) = ws.split();
        // Say hello again first, so the hub knows this device.
        let again = hello.lock().unwrap().clone();
        if let Some(h) = again {
            let _ = tx
                .send(Message::Binary(
                    encode(&ClientEnvelope::new(0, Req::Hello(h))).into(),
                ))
                .await;
        }
        // Then what this app watches, so live events reach it again.
        let again = watch.lock().unwrap().clone();
        if let Some(w) = again {
            let _ = tx
                .send(Message::Binary(
                    encode(&ClientEnvelope::new(0, Req::Watch(w))).into(),
                ))
                .await;
        }
        // Anything sent while disconnected may have been missed.
        changed.notify_one();
        loop {
            tokio::select! {
                out = outgoing.recv() => {
                    let Some(frame) = out else { return };
                    if tx.send(Message::Binary(frame.into())).await.is_err() {
                        break;
                    }
                }
                msg = rx.next() => {
                    let frame = match msg {
                        Some(Ok(Message::Binary(b))) => b,
                        Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                        Some(Ok(_)) => continue,
                    };
                    let Ok(env) = tab_protocol::decode_server(&frame) else { continue };
                    if env.request_id == 0 {
                        match push_kind(&env.body) {
                            Push::Changed => changed.notify_one(),
                            Push::Live => {
                                if let Some(sink) = &*live.lock().unwrap() {
                                    let _ = sink.send(env);
                                }
                            }
                            Push::Ignore => {}
                        }
                        continue;
                    }
                    if let (Some(w), Some(body)) =
                        (waiting.lock().unwrap().remove(&env.request_id), env.body)
                    {
                        let _ = w.try_send(body);
                    }
                }
            }
        }
        connected.store(false, Ordering::Relaxed);
        tracing::warn!("cloud hub: disconnected; reconnecting");
        fail_all(&waiting);
        if wait_or_closed(&mut outgoing, &waiting, Duration::from_secs(1)).await {
            return;
        }
    }
}

fn fail_all(waiting: &Waiting) {
    // Dropping the senders fails the waiting requests.
    waiting.lock().unwrap().clear();
}

/// Waits `d`, dropping frames queued meanwhile (their requests have failed).
/// Returns true when the link is gone.
async fn wait_or_closed(
    outgoing: &mut mpsc::UnboundedReceiver<Vec<u8>>,
    waiting: &Waiting,
    d: Duration,
) -> bool {
    let sleep = tokio::time::sleep(d);
    tokio::pin!(sleep);
    loop {
        tokio::select! {
            _ = &mut sleep => return false,
            f = outgoing.recv() => match f {
                None => return true,
                // Not connected: fail it now rather than at the timeout.
                Some(_) => fail_all(waiting),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_urls() {
        assert_eq!(
            sync_url("https://api.tabqueue.com", "ab"),
            "wss://api.tabqueue.com/v1/hubs/ab/sync"
        );
        assert_eq!(
            sync_url("http://127.0.0.1:8787/", "ab"),
            "ws://127.0.0.1:8787/v1/hubs/ab/sync"
        );
    }

    #[test]
    fn live_pushes_go_to_the_inbox() {
        let steered = Some(Res::Steered(pb::Steered::default()));
        let live = Some(Res::Live(pb::Live::default()));
        let watched = Some(Res::Watched(pb::Watched::default()));
        for b in [steered, live, watched] {
            assert_eq!(push_kind(&b), Push::Live);
        }
        assert_eq!(
            push_kind(&Some(Res::Changes(pb::Changes::default()))),
            Push::Changed
        );
        assert_eq!(push_kind(&None), Push::Ignore);
    }
}
