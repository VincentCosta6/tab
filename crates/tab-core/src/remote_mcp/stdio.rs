//! Local MCP servers: a command tab runs as its own child and talks to over
//! stdio (newline-delimited JSON-RPC).
//!
//! Agents never start them. Each chat reaches one through tab's MCP server
//! at `/mcp/<id>`, the same as a remote server, so the person's choice for
//! each tool applies and strict chats can use them. One process serves every
//! chat: tab does the `initialize` handshake once, answers each chat's own
//! `initialize` from it, and gives each request a fresh id so replies to two
//! chats can't cross.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};

use super::{LocalCommand, PROTOCOL_VERSION};
use crate::sandbox::Guarded;

/// How long a server gets to start and answer `initialize`. `npx` and
/// `uvx` may download the package first.
const START_TIMEOUT: Duration = Duration::from_secs(90);
/// How long one request may take (a tool that does real work).
pub const CALL_TIMEOUT: Duration = Duration::from_secs(600);

/// What a process was started with. A change restarts it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spec {
    pub command: LocalCommand,
    pub env: BTreeMap<String, String>,
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>;
/// One server's process, while it runs (a lock per server, so starting one
/// doesn't hold up the others).
type Slot = Arc<tokio::sync::Mutex<Option<Arc<Proc>>>>;

struct Proc {
    spec: Spec,
    tx: mpsc::UnboundedSender<String>,
    pending: Pending,
    next: AtomicU64,
    /// The server's `initialize` result.
    init: Value,
    dead: Arc<AtomicBool>,
    stderr: Arc<Mutex<String>>,
    /// Dropping it ends the process and anything it started.
    _child: Mutex<Guarded>,
}

impl Proc {
    fn alive(&self) -> bool {
        !self.dead.load(Ordering::Relaxed)
    }

    /// Sends one request and waits for its reply (the whole message).
    async fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> anyhow::Result<Value> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        if self.tx.send(msg.to_string()).is_err() {
            self.pending.lock().unwrap().remove(&id);
            anyhow::bail!("{}", self.gone());
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(_)) => anyhow::bail!("{}", self.gone()),
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                let cancel = json!({ "jsonrpc": "2.0", "method": "notifications/cancelled", "params": { "requestId": id, "reason": "timed out" } });
                let _ = self.tx.send(cancel.to_string());
                anyhow::bail!(
                    "the server took longer than {}s to answer",
                    timeout.as_secs()
                )
            }
        }
    }

    fn gone(&self) -> String {
        stopped_message(&self.stderr.lock().unwrap())
    }
}

fn stopped_message(stderr: &str) -> String {
    let tail = stderr.trim();
    if tail.is_empty() {
        "the server stopped".to_string()
    } else {
        format!("the server stopped: {tail}")
    }
}

/// The local servers tab is running, by connection id.
#[derive(Default)]
pub struct Children {
    slots: Mutex<HashMap<String, Slot>>,
}

impl Children {
    /// Stops a server (it starts again on the next request).
    pub fn stop(&self, id: &str) {
        // Requests in flight keep their process until they finish.
        self.slots.lock().unwrap().remove(id);
    }

    /// Stops every server not in `keep`.
    pub fn retain(&self, keep: impl Fn(&str) -> bool) {
        self.slots.lock().unwrap().retain(|id, _| keep(id));
    }

    /// Whether `id` is running now.
    pub fn running(&self, id: &str) -> bool {
        let slot = self.slots.lock().unwrap().get(id).cloned();
        slot.and_then(|s| {
            s.try_lock()
                .ok()
                .and_then(|p| p.as_ref().map(|p| p.alive()))
        })
        .unwrap_or(false)
    }

    /// The running process for `id`, started (or restarted, when its
    /// command changed or it stopped) as needed.
    async fn get(&self, id: &str, spec: &Spec) -> anyhow::Result<Arc<Proc>> {
        let slot = self
            .slots
            .lock()
            .unwrap()
            .entry(id.to_string())
            .or_default()
            .clone();
        let mut cur = slot.lock().await;
        if let Some(p) = cur.as_ref()
            && p.alive()
            && &p.spec == spec
        {
            return Ok(p.clone());
        }
        *cur = None;
        let p = Arc::new(start(spec).await?);
        *cur = Some(p.clone());
        Ok(p)
    }

    /// One request in a fresh exchange, as tab itself makes them (listing
    /// tools). Returns the reply's `result`.
    pub async fn rpc(
        &self,
        id: &str,
        spec: &Spec,
        method: &str,
        params: Value,
    ) -> anyhow::Result<Value> {
        let p = self.get(id, spec).await?;
        let reply = p.request(method, params, CALL_TIMEOUT).await?;
        if let Some(e) = reply.get("error") {
            anyhow::bail!(
                "{}",
                e["message"]
                    .as_str()
                    .unwrap_or("the server returned an error")
            );
        }
        Ok(reply["result"].clone())
    }

    /// Forwards one message from a chat. Requests get their reply (with the
    /// chat's own id); notifications get `None`.
    pub async fn forward(
        &self,
        id: &str,
        spec: &Spec,
        msg: &Value,
    ) -> anyhow::Result<Option<Value>> {
        let p = self.get(id, spec).await?;
        let Some(req_id) = msg.get("id").filter(|v| !v.is_null()).cloned() else {
            // The chat's `initialized` and cancellations of ids it never
            // had: the server only ever saw tab's.
            return Ok(None);
        };
        let method = msg["method"].as_str().unwrap_or_default();
        if method == "initialize" {
            return Ok(Some(
                json!({ "jsonrpc": "2.0", "id": req_id, "result": p.init }),
            ));
        }
        let mut reply = p
            .request(
                method,
                msg.get("params").cloned().unwrap_or(json!({})),
                CALL_TIMEOUT,
            )
            .await?;
        reply["id"] = req_id;
        Ok(Some(reply))
    }
}

/// Starts the server and does the `initialize` handshake.
async fn start(spec: &Spec) -> anyhow::Result<Proc> {
    let c = &spec.command;
    let program = resolve(&c.command, c.cwd.as_deref())?;
    let mut cmd = crate::agents::command(&program);
    cmd.args(&c.args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    // tab's own tokens stay out; the server's come from its settings.
    crate::sandbox::env::scrub(&mut cmd, crate::sandbox::Trust::Person);
    cmd.envs(&spec.env);
    if let Some(dir) = c
        .cwd
        .as_deref()
        .filter(|d| std::path::Path::new(d).is_dir())
    {
        cmd.current_dir(dir);
    }
    crate::sandbox::tree::prepare(&mut cmd);
    let mut child = cmd
        .spawn()
        .map_err(|e| anyhow::anyhow!("couldn't start {}: {e}", c.command))?;
    let stdin = child.stdin.take().expect("piped");
    let stdout = child.stdout.take().expect("piped");
    let mut stderr_pipe = child.stderr.take().expect("piped");
    let child = Guarded::new(child, None);

    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let pending: Pending = Default::default();
    let dead = Arc::new(AtomicBool::new(false));
    let stderr = Arc::new(Mutex::new(String::new()));

    // Writer: one line per message.
    tokio::spawn(async move {
        let mut stdin = stdin;
        while let Some(line) = rx.recv().await {
            if stdin
                .write_all(format!("{line}\n").as_bytes())
                .await
                .is_err()
            {
                break;
            }
            let _ = stdin.flush().await;
        }
    });
    // The last bit of stderr, for errors.
    {
        let stderr = stderr.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            while let Ok(n) = stderr_pipe.read(&mut buf).await {
                if n == 0 {
                    break;
                }
                let mut s = stderr.lock().unwrap();
                s.push_str(&String::from_utf8_lossy(&buf[..n]));
                if s.len() > 2000 {
                    let cut = s.len() - 1000;
                    let cut = (cut..s.len()).find(|i| s.is_char_boundary(*i)).unwrap_or(0);
                    s.drain(..cut);
                }
            }
        });
    }
    // Reader: replies go to whoever waits; the server's own requests get
    // a polite no (tab offers no sampling, roots or elicitation).
    {
        let pending = pending.clone();
        let dead = dead.clone();
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(v) = serde_json::from_str::<Value>(line.trim()) else {
                    continue;
                };
                for m in match v {
                    Value::Array(a) => a,
                    v => vec![v],
                } {
                    route(m, &pending, &tx);
                }
            }
            dead.store(true, Ordering::Relaxed);
            // Waiters see the process is gone.
            pending.lock().unwrap().clear();
        });
    }

    let p = Proc {
        spec: spec.clone(),
        tx,
        pending,
        next: AtomicU64::new(1),
        init: Value::Null,
        dead,
        stderr,
        _child: Mutex::new(child),
    };
    let params = json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": {},
        "clientInfo": { "name": "tab", "version": env!("CARGO_PKG_VERSION") }
    });
    let reply = p
        .request("initialize", params, START_TIMEOUT)
        .await
        .map_err(|e| anyhow::anyhow!("{} didn't start: {e}", c.command))?;
    if let Some(e) = reply.get("error") {
        anyhow::bail!(
            "{} didn't start: {}",
            c.command,
            e["message"].as_str().unwrap_or("it returned an error")
        );
    }
    let _ =
        p.tx.send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }).to_string());
    Ok(Proc {
        init: reply["result"].clone(),
        ..p
    })
}

fn route(m: Value, pending: &Pending, tx: &mpsc::UnboundedSender<String>) {
    let is_reply = m.get("result").is_some() || m.get("error").is_some();
    if is_reply {
        if let Some(id) = m["id"].as_u64()
            && let Some(w) = pending.lock().unwrap().remove(&id)
        {
            let _ = w.send(m);
        }
        return;
    }
    // A request from the server (it has an id and a method).
    if let (Some(id), Some(method)) = (m.get("id"), m["method"].as_str()) {
        let reply = if method == "ping" {
            json!({ "jsonrpc": "2.0", "id": id, "result": {} })
        } else {
            json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": format!("tab doesn't offer {method}") } })
        };
        let _ = tx.send(reply.to_string());
    }
    // Notifications (logs, list changes) aren't passed on.
}

/// Finds the program: as given when it's a path, else on PATH (with
/// Windows' `.cmd` shims, which `npx` is).
fn resolve(command: &str, cwd: Option<&str>) -> anyhow::Result<std::path::PathBuf> {
    let p = std::path::Path::new(command);
    if p.is_absolute() {
        anyhow::ensure!(p.exists(), "{command} isn't on this computer");
        return Ok(p.to_path_buf());
    }
    if command.contains('/') || command.contains('\\') {
        let base = cwd.map(std::path::PathBuf::from).unwrap_or_default();
        return Ok(base.join(p));
    }
    which::which(command).map_err(|_| {
        anyhow::anyhow!("{command} isn't installed on this computer (or isn't on tab's PATH)")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny MCP server: answers initialize, tools/list and tools/call,
    /// asks tab for roots once, and echoes an env var.
    fn fake_server() -> Option<Spec> {
        let script = r#"
const rl = require('readline').createInterface({ input: process.stdin });
let asked = false;
rl.on('line', (l) => {
  const m = JSON.parse(l);
  const out = (o) => process.stdout.write(JSON.stringify(o) + '\n');
  if (m.method === 'initialize') out({ jsonrpc: '2.0', id: m.id, result: { protocolVersion: '2025-06-18', capabilities: { tools: {} }, serverInfo: { name: 'fake' } } });
  else if (m.method === 'tools/list') {
    if (!asked) { asked = true; out({ jsonrpc: '2.0', id: 'srv-1', method: 'roots/list' }); }
    out({ jsonrpc: '2.0', method: 'notifications/message', params: { level: 'info', data: 'hi' } });
    out({ jsonrpc: '2.0', id: m.id, result: { tools: [{ name: 'echo', annotations: { readOnlyHint: true } }] } });
  }
  else if (m.method === 'tools/call') out({ jsonrpc: '2.0', id: m.id, result: { content: [{ type: 'text', text: process.env.FAKE_KEY + ':' + m.params.arguments.x }] } });
  else if (m.id !== undefined && m.method) out({ jsonrpc: '2.0', id: m.id, error: { code: -32601, message: 'no' } });
});
"#;
        let node = which::which("node").ok()?;
        Some(Spec {
            command: LocalCommand {
                command: node.display().to_string(),
                args: vec!["-e".into(), script.into()],
                env: vec!["FAKE_KEY".into()],
                cwd: None,
            },
            env: BTreeMap::from([("FAKE_KEY".to_string(), "k1".to_string())]),
        })
    }

    #[tokio::test]
    async fn runs_a_local_server_for_every_chat() {
        let Some(spec) = fake_server() else {
            eprintln!("no node; skipping");
            return;
        };
        let c = Children::default();
        let tools = c.rpc("fake", &spec, "tools/list", json!({})).await.unwrap();
        assert_eq!(tools["tools"][0]["name"], "echo");

        // A chat's initialize is answered from tab's own handshake.
        let init = c
            .forward(
                "fake",
                &spec,
                &json!({ "jsonrpc": "2.0", "id": "a", "method": "initialize", "params": {} }),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(init["id"], "a");
        assert_eq!(init["result"]["serverInfo"]["name"], "fake");
        assert!(
            c.forward(
                "fake",
                &spec,
                &json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })
            )
            .await
            .unwrap()
            .is_none()
        );

        // Two chats using the same ids get their own answers.
        let call = |id: i64, x: &str| json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call", "params": { "name": "echo", "arguments": { "x": x } } });
        let (one, two) = (call(7, "one"), call(7, "two"));
        let (a, b) = tokio::join!(
            c.forward("fake", &spec, &one),
            c.forward("fake", &spec, &two)
        );
        let (a, b) = (a.unwrap().unwrap(), b.unwrap().unwrap());
        assert_eq!(a["id"], 7);
        assert_eq!(a["result"]["content"][0]["text"], "k1:one");
        assert_eq!(b["result"]["content"][0]["text"], "k1:two");
        assert!(c.running("fake"));

        // New settings restart it.
        let mut changed = spec.clone();
        changed.env.insert("FAKE_KEY".into(), "k2".into());
        let r = c
            .forward("fake", &changed, &call(1, "x"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(r["result"]["content"][0]["text"], "k2:x");

        c.stop("fake");
        assert!(!c.running("fake"));
    }

    #[tokio::test]
    async fn says_what_went_wrong_when_it_cant_start() {
        let c = Children::default();
        let spec = Spec {
            command: LocalCommand {
                command: "tab-no-such-mcp-server".into(),
                ..Default::default()
            },
            env: Default::default(),
        };
        let e = c
            .rpc("x", &spec, "tools/list", json!({}))
            .await
            .unwrap_err();
        assert!(e.to_string().contains("isn't installed"), "{e}");
    }
}
