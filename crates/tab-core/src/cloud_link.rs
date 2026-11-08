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
//! A `ws://` address (a local `wrangler dev`) works too, for dev builds.
//!
//! Files travel beside the socket, over HTTPS at the hub's `/files`
//! (`PUT /v1/hubs/{hub_id}/files`, `GET|HEAD /v1/hubs/{hub_id}/files/{hex}`),
//! as their own bytes. Those calls block too.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::future::BoxFuture;
use futures_util::{SinkExt, StreamExt};
use tab_protocol::pb::{self, client_envelope::Body as Req, server_envelope::Body as Res};
use tab_protocol::{ClientEnvelope, encode};
use tokio::sync::{Notify, mpsc};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

/// How long one request may take before it's given up on.
const TIMEOUT: Duration = Duration::from_secs(30);
/// How long one file may take to go up or come down (up to 50 MiB).
const FILE_TIMEOUT: Duration = Duration::from_secs(180);
/// Waits between reconnects, growing to the last.
const BACKOFF: [u64; 5] = [1, 2, 5, 10, 30];

/// Gets a current access token (refreshing it if it's about to expire).
pub type TokenSource = Arc<dyn Fn() -> BoxFuture<'static, anyhow::Result<String>> + Send + Sync>;

type Waiting = Arc<Mutex<HashMap<u64, std::sync::mpsc::SyncSender<Res>>>>;

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
    connected: Arc<std::sync::atomic::AtomicBool>,
    token: TokenSource,
    /// Where file calls run; the caller's thread waits on them.
    rt: tokio::runtime::Handle,
}

/// What a cloud hub said about a file it stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredFile {
    pub hash: String,
    pub size: u64,
}

/// A hub's sync address to the address of its files.
pub fn files_url(sync: &str) -> String {
    let base = sync.strip_suffix("/sync").unwrap_or(sync);
    let http = if let Some(rest) = base.strip_prefix("wss://") {
        format!("https://{rest}")
    } else if let Some(rest) = base.strip_prefix("ws://") {
        format!("http://{rest}")
    } else {
        base.to_string()
    };
    format!("{http}/files")
}

fn http() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .user_agent(concat!("tab-desktop/", env!("CARGO_PKG_VERSION")))
            .timeout(FILE_TIMEOUT)
            .build()
            .expect("an HTTP client")
    })
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
            connected: Default::default(),
            token: token.clone(),
            rt: tokio::runtime::Handle::current(),
        });
        tokio::spawn(run(
            link.url.clone(),
            token,
            rx,
            link.waiting.clone(),
            link.changed.clone(),
            link.hello.clone(),
            link.connected.clone(),
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

/// Files in the cloud hub.
impl CloudLink {
    /// Runs `f` on the runtime and waits for it here, so it can be called
    /// from any thread (even one of the runtime's, as `request` is).
    fn wait<T: Send + 'static>(
        &self,
        f: impl std::future::Future<Output = anyhow::Result<T>> + Send + 'static,
    ) -> anyhow::Result<T> {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        self.rt.spawn(async move {
            let _ = tx.send(f.await);
        });
        rx.recv_timeout(FILE_TIMEOUT + Duration::from_secs(5))
            .map_err(|_| anyhow::anyhow!("cloud hub: no answer about a file"))?
    }

    fn file_request(
        &self,
        method: reqwest::Method,
        hash: Option<&str>,
    ) -> anyhow::Result<
        impl std::future::Future<Output = anyhow::Result<reqwest::RequestBuilder>> + Send + 'static,
    > {
        let base = files_url(&self.url);
        let url = match hash {
            Some(h) => {
                let hex = tab_domain::blob::hex(h)
                    .ok_or_else(|| anyhow::anyhow!("not a file hash: {h}"))?;
                format!("{base}/{hex}")
            }
            None => base,
        };
        let token = self.token.clone();
        Ok(async move {
            let token = token().await?;
            Ok(http()
                .request(method, url)
                .header("authorization", format!("Bearer {token}")))
        })
    }

    /// Stores a file in the cloud hub. The hub names it by its hash, so
    /// sending the same bytes again changes nothing.
    pub fn put_file(&self, bytes: Vec<u8>) -> anyhow::Result<StoredFile> {
        let req = self.file_request(reqwest::Method::PUT, None)?;
        self.wait(async move {
            let res = req
                .await?
                .header("content-type", "application/octet-stream")
                .header("accept", tab_protocol::cloud::CONTENT_TYPE)
                .body(bytes)
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("cloud hub: {}", e.without_url()))?;
            let status = res.status().as_u16();
            let body = res.bytes().await?;
            let stored: tab_protocol::cloud::StoredFile = crate::cloud::interpret(status, &body)
                .map_err(|e| anyhow::anyhow!("cloud hub: {e}"))?;
            Ok(StoredFile {
                hash: stored.hash,
                size: stored.size,
            })
        })
    }

    /// A file's bytes from the cloud hub, or `None` if it doesn't have it.
    /// The bytes are checked against the hash.
    pub fn get_file(&self, hash: &str) -> anyhow::Result<Option<Vec<u8>>> {
        let req = self.file_request(reqwest::Method::GET, Some(hash))?;
        let want = hash.to_string();
        self.wait(async move {
            let res = req
                .await?
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("cloud hub: {}", e.without_url()))?;
            match res.status().as_u16() {
                404 => return Ok(None),
                200 => {}
                s => anyhow::bail!("cloud hub answered {s} for a file"),
            }
            let bytes = res.bytes().await?.to_vec();
            anyhow::ensure!(
                tab_domain::blob::hash(&bytes) == want,
                "cloud hub: a file came back with other bytes than its hash says"
            );
            Ok(Some(bytes))
        })
    }

    /// Whether the cloud hub has this file.
    pub fn has_file(&self, hash: &str) -> anyhow::Result<bool> {
        let req = self.file_request(reqwest::Method::HEAD, Some(hash))?;
        self.wait(async move {
            let res = req
                .await?
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("cloud hub: {}", e.without_url()))?;
            match res.status().as_u16() {
                200 => Ok(true),
                404 => Ok(false),
                s => anyhow::bail!("cloud hub answered {s} for a file"),
            }
        })
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

/// Keeps the socket up for as long as the link exists.
#[allow(clippy::too_many_arguments)]
async fn run(
    url: String,
    token: TokenSource,
    mut outgoing: mpsc::UnboundedReceiver<Vec<u8>>,
    waiting: Waiting,
    changed: Arc<Notify>,
    hello: Arc<Mutex<Option<pb::Hello>>>,
    connected: Arc<std::sync::atomic::AtomicBool>,
) {
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
                        if matches!(env.body, Some(Res::Changes(_))) {
                            changed.notify_one();
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
    fn file_urls() {
        assert_eq!(
            files_url("wss://api.tabqueue.com/v1/hubs/ab/sync"),
            "https://api.tabqueue.com/v1/hubs/ab/files"
        );
        assert_eq!(
            files_url(&sync_url("http://127.0.0.1:8787", "ab")),
            "http://127.0.0.1:8787/v1/hubs/ab/files"
        );
    }
}
