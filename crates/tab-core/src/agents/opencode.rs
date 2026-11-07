//! OpenCode adapter: tab starts (or attaches to) an `opencode serve` HTTP
//! server, creates one OpenCode session per tab session, sends prompts with
//! `prompt_async`, and follows the server's SSE event stream.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt;
use serde_json::{Value, json};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use super::{AgentHub, Decision, default_command, resolve};
use crate::model::{AgentKind, Session, SessionStatus};

/// Turn state for one OpenCode session, keyed by OpenCode's session id.
#[derive(Default)]
struct Turn {
    tab_session: String,
    /// Message id → role, so the person's own message parts are skipped.
    roles: HashMap<String, String>,
    /// Text parts not yet stored, in arrival order.
    pending: Vec<(String, String)>,
    tools_done: std::collections::HashSet<String>,
    /// Assistant messages whose cost is already recorded.
    costed: std::collections::HashSet<String>,
    error: Option<String>,
    done: Arc<Notify>,
}

pub(super) struct Server {
    base: String,
    directory: String,
    client: reqwest::Client,
    turns: Arc<Mutex<HashMap<String, Turn>>>,
    child: Mutex<Option<crate::sandbox::Guarded>>,
    cancel: CancellationToken,
}

impl Server {
    pub(super) fn shutdown(&self) {
        self.cancel.cancel();
        if let Some(mut c) = self.child.lock().unwrap().take() {
            c.kill_tree();
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base.trim_end_matches('/'))
    }

    fn q(&self) -> [(&str, &str); 1] {
        [("directory", self.directory.as_str())]
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .map(|a| a.port())
        .unwrap_or(4096)
}

async fn server(hub: &Arc<AgentHub>) -> anyhow::Result<Arc<Server>> {
    let mut slot = hub.opencode.lock().await;
    if let Some(s) = slot.as_ref()
        && !s.cancel.is_cancelled()
    {
        return Ok(s.clone());
    }
    let cfg = hub.config();
    let directory = hub.workdir();
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .build()?;
    let (base, child) = match cfg.opencode_url.as_deref().filter(|u| !u.trim().is_empty()) {
        Some(url) => (url.to_string(), None),
        None => {
            let exe = resolve(
                cfg.command
                    .as_deref()
                    .filter(|c| !c.trim().is_empty() && cfg.kind == Some(AgentKind::OpenCode))
                    .unwrap_or(default_command(AgentKind::OpenCode)),
            )?;
            let port = free_port();
            // One server serves every chat, so it runs under a person's
            // chat policy.
            let policy = crate::sandbox::current().chat_policy();
            let args: Vec<std::ffi::OsString> = ["serve", "--hostname", "127.0.0.1", "--port"]
                .iter()
                .map(Into::into)
                .chain([port.to_string().into()])
                .collect();
            let mut prepared = crate::sandbox::current()
                .command(&exe, &args, std::path::Path::new(&directory), &policy)
                .await?;
            let cmd = &mut prepared.cmd;
            cmd.stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            if let Some(mcp) = hub.mcp() {
                // Inline config: adds tab's MCP server without touching the
                // person's own opencode.json.
                let config = json!({
                    "mcp": {
                        "tab": {
                            "type": "remote",
                            "url": mcp.url,
                            "enabled": true,
                            "headers": { "Authorization": format!("Bearer {}", mcp.token) }
                        }
                    }
                });
                cmd.env("OPENCODE_CONFIG_CONTENT", config.to_string());
            }
            let child = prepared.spawn().map_err(|e| {
                anyhow::anyhow!("starting OpenCode ({}) failed: {e}", exe.display())
            })?;
            (format!("http://127.0.0.1:{port}"), Some(child))
        }
    };

    // Wait for it to answer.
    let health = format!("{}/global/health", base.trim_end_matches('/'));
    let mut ok = false;
    for _ in 0..60 {
        if client
            .get(&health)
            .send()
            .await
            .is_ok_and(|r| r.status().is_success())
        {
            ok = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    anyhow::ensure!(ok, "OpenCode server at {base} did not come up");

    let s = Arc::new(Server {
        base,
        directory,
        client,
        turns: Arc::new(Mutex::new(HashMap::new())),
        child: Mutex::new(child),
        cancel: CancellationToken::new(),
    });
    tokio::spawn(follow_events(hub.clone(), s.clone()));
    *slot = Some(s.clone());
    Ok(s)
}

/// Reads `GET /event` (SSE) for as long as the server lives, reconnecting on drops.
async fn follow_events(hub: Arc<AgentHub>, s: Arc<Server>) {
    while !s.cancel.is_cancelled() {
        let resp = s
            .client
            .get(s.url("/event"))
            .query(&s.q())
            .header("accept", "text/event-stream")
            .send()
            .await;
        let mut stream = match resp {
            Ok(r) if r.status().is_success() => r.bytes_stream(),
            _ => {
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        };
        let mut buf = String::new();
        loop {
            let chunk = tokio::select! {
                c = stream.next() => c,
                _ = s.cancel.cancelled() => return,
            };
            let Some(Ok(bytes)) = chunk else { break };
            buf.push_str(&String::from_utf8_lossy(&bytes).replace("\r\n", "\n"));
            while let Some(end) = buf.find("\n\n") {
                let frame: String = buf.drain(..end + 2).collect();
                let data: String = frame
                    .lines()
                    .filter_map(|l| l.strip_prefix("data:"))
                    .map(str::trim_start)
                    .collect::<Vec<_>>()
                    .join("\n");
                if let Ok(v) = serde_json::from_str::<Value>(&data) {
                    handle_event(&hub, &s, &v);
                }
            }
        }
    }
}

fn flush(hub: &AgentHub, turn: &mut Turn) {
    for (_, text) in turn.pending.drain(..) {
        if !text.trim().is_empty() {
            hub.post(&turn.tab_session, "assistant", "text", &text, Value::Null);
        }
    }
}

fn handle_event(hub: &Arc<AgentHub>, s: &Arc<Server>, v: &Value) {
    let props = &v["properties"];
    let session_of = |p: &Value| {
        p["sessionID"]
            .as_str()
            .or(p["info"]["sessionID"].as_str())
            .or(p["part"]["sessionID"].as_str())
            .map(str::to_string)
    };
    let Some(oc_session) = session_of(props) else {
        return;
    };
    let mut turns = s.turns.lock().unwrap();
    let Some(turn) = turns.get_mut(&oc_session) else {
        return;
    };
    let sid = turn.tab_session.clone();
    match v["type"].as_str().unwrap_or_default() {
        "message.updated" => {
            if let (Some(id), Some(role)) =
                (props["info"]["id"].as_str(), props["info"]["role"].as_str())
            {
                turn.roles.insert(id.into(), role.into());
                let info = &props["info"];
                if role == "assistant"
                    && !info["time"]["completed"].is_null()
                    && turn.costed.insert(id.into())
                {
                    let t = &info["tokens"];
                    let n = |v: &Value| v.as_i64().unwrap_or(0);
                    let u = crate::store::NewUsage {
                        purpose: "agent",
                        session_id: Some(sid.clone()),
                        agent: "opencode".into(),
                        model: info["modelID"].as_str().map(str::to_string),
                        billing: "api",
                        cost_usd: info["cost"].as_f64().unwrap_or(0.0),
                        input_tokens: n(&t["input"])
                            + n(&t["cache"]["read"])
                            + n(&t["cache"]["write"]),
                        output_tokens: n(&t["output"]) + n(&t["reasoning"]),
                    };
                    if let Err(e) = hub.store.usage_add(&u, chrono::Utc::now()) {
                        tracing::warn!(error = %e, "couldn't store usage");
                    }
                }
            }
        }
        "message.part.delta" => {
            let msg = props["messageID"].as_str().unwrap_or_default();
            if props["field"] == "text"
                && turn.roles.get(msg).map(String::as_str) != Some("user")
                && let Some(d) = props["delta"].as_str()
            {
                hub.delta(&sid, d);
            }
        }
        "message.part.updated" => {
            let part = &props["part"];
            let msg = part["messageID"].as_str().unwrap_or_default();
            if turn.roles.get(msg).map(String::as_str) == Some("user") {
                return;
            }
            let pid = part["id"].as_str().unwrap_or_default().to_string();
            match part["type"].as_str() {
                Some("text") => {
                    let text = part["text"].as_str().unwrap_or_default().to_string();
                    match turn.pending.iter_mut().find(|(id, _)| *id == pid) {
                        Some(p) => p.1 = text,
                        None => turn.pending.push((pid, text)),
                    }
                }
                Some("tool") => {
                    let state = &part["state"];
                    let status = state["status"].as_str().unwrap_or_default();
                    if matches!(status, "completed" | "error") && turn.tools_done.insert(pid) {
                        flush(hub, turn);
                        let tool = part["tool"].as_str().unwrap_or("tool");
                        hub.post(
                            &sid,
                            "assistant",
                            "tool",
                            &super::summarize_tool(tool, &state["input"]),
                            json!({ "tool": tool, "input": state["input"], "title": state["title"] }),
                        );
                        let output = state["output"]
                            .as_str()
                            .or(state["error"].as_str())
                            .unwrap_or_default();
                        if status == "completed" && crate::links::is_creator(tool, &state["input"])
                        {
                            hub.attach_created_links(&sid, output);
                        }
                        hub.post(
                            &sid,
                            "tool",
                            "tool_result",
                            &output.chars().take(4000).collect::<String>(),
                            json!({ "is_error": status == "error" }),
                        );
                    }
                }
                _ => {}
            }
        }
        "permission.asked" | "permission.updated" => {
            let Some(req) = props["id"].as_str().map(str::to_string) else {
                return;
            };
            let permission = props["permission"]
                .as_str()
                .or(props["type"].as_str())
                .unwrap_or("permission")
                .to_string();
            let patterns = props["patterns"].clone();
            let metadata = props["metadata"].clone();
            let summary = match patterns
                .as_array()
                .and_then(|p| p.first())
                .and_then(|p| p.as_str())
            {
                Some(p) => format!("{permission}: {p}"),
                None => props["title"]
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or(permission.clone()),
            };
            let (hub, s) = (hub.clone(), s.clone());
            tokio::spawn(async move {
                let d = hub
                    .request_approval(
                        &sid,
                        &permission,
                        summary,
                        json!({ "patterns": patterns, "metadata": metadata }),
                    )
                    .await;
                let (reply, message) = match d {
                    Decision::Allow | Decision::Answer(_) => ("once", None),
                    Decision::AllowAlways => ("always", None),
                    Decision::Deny(m) => ("reject", Some(m)),
                };
                let _ = s
                    .client
                    .post(s.url(&format!("/permission/{req}/reply")))
                    .query(&s.q())
                    .json(&json!({ "reply": reply, "message": message }))
                    .send()
                    .await;
            });
        }
        "session.error" => {
            let e = &props["error"];
            turn.error = Some(
                e["data"]["message"]
                    .as_str()
                    .or(e["message"].as_str())
                    .or(e["name"].as_str())
                    .unwrap_or("OpenCode reported an error")
                    .to_string(),
            );
            turn.done.notify_one();
        }
        "session.idle" => {
            flush(hub, turn);
            turn.done.notify_one();
        }
        "session.status" if props["status"]["type"] == "idle" => {
            flush(hub, turn);
            turn.done.notify_one();
        }
        _ => {}
    }
}

pub(super) async fn run_turn(
    hub: &Arc<AgentHub>,
    session: &Session,
    prompt: &str,
    system: &str,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    let s = server(hub).await?;
    let oc_id = match &session.agent_session_id {
        Some(id) => id.clone(),
        None => {
            let r = s
                .client
                .post(s.url("/session"))
                .query(&s.q())
                .json(&json!({ "title": session.title }))
                .send()
                .await?
                .error_for_status()?;
            let v: Value = r.json().await?;
            let id = v["id"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("OpenCode did not return a session id"))?
                .to_string();
            hub.update(&session.id, None, Some(&id));
            id
        }
    };
    let done = Arc::new(Notify::new());
    s.turns.lock().unwrap().insert(
        oc_id.clone(),
        Turn {
            tab_session: session.id.clone(),
            done: done.clone(),
            ..Default::default()
        },
    );

    let mut body = json!({
        "parts": [{ "type": "text", "text": prompt }],
        "system": system,
    });
    if let Some((provider, model)) = hub
        .config()
        .model
        .as_deref()
        .and_then(|m| m.split_once('/'))
    {
        body["model"] = json!({ "providerID": provider, "modelID": model });
    }
    s.client
        .post(s.url(&format!("/session/{oc_id}/prompt_async")))
        .query(&s.q())
        .json(&body)
        .send()
        .await?
        .error_for_status()?;

    // Steering: more prompts to the same session while it works. OpenCode
    // reads the new message at its next step, and the turn ends when the
    // session goes idle with nothing left queued.
    let mut steer_rx = hub.steering_open(&session.id);
    let steer = {
        let (s, oc_id, mut body) = (s.clone(), oc_id.clone(), body.clone());
        tokio::spawn(async move {
            while let Some(text) = steer_rx.recv().await {
                body["parts"] = json!([{ "type": "text", "text": text }]);
                let sent = s
                    .client
                    .post(s.url(&format!("/session/{oc_id}/prompt_async")))
                    .query(&s.q())
                    .json(&body)
                    .send()
                    .await;
                if let Err(e) = sent {
                    tracing::warn!(error = %e, "steering an OpenCode session failed");
                }
            }
        })
    };

    tokio::select! {
        _ = done.notified() => {}
        _ = cancel.cancelled() => {
            let _ = s.client.post(s.url(&format!("/session/{oc_id}/abort"))).query(&s.q()).send().await;
        }
    }
    hub.steering_close(&session.id);
    steer.abort();
    let turn = s.turns.lock().unwrap().remove(&oc_id);
    if let Some(mut t) = turn {
        flush(hub, &mut t);
        if let Some(e) = t.error {
            anyhow::bail!(e);
        }
    }
    if !cancel.is_cancelled() {
        hub.update(&session.id, Some(SessionStatus::Idle), None);
    }
    Ok(())
}

/// One-shot completion on a throwaway OpenCode session, for small jobs like
/// suggesting a reply. Uses whichever model tab is configured with.
pub(super) async fn complete(
    hub: &Arc<AgentHub>,
    system: &str,
    prompt: &str,
) -> anyhow::Result<String> {
    let s = server(hub).await?;
    let v: Value = s
        .client
        .post(s.url("/session"))
        .query(&s.q())
        .json(&json!({ "title": "tab: suggestion" }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let id = v["id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("OpenCode did not return a session id"))?
        .to_string();
    let mut body = json!({
        "parts": [{ "type": "text", "text": prompt }],
        "system": system,
        "tools": {},
    });
    if let Some((provider, model)) = hub
        .config()
        .model
        .as_deref()
        .and_then(|m| m.split_once('/'))
    {
        body["model"] = json!({ "providerID": provider, "modelID": model });
    }
    let out = tokio::time::timeout(Duration::from_secs(60), async {
        s.client
            .post(s.url(&format!("/session/{id}/message")))
            .query(&s.q())
            .json(&body)
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await
    })
    .await;
    let _ = s
        .client
        .delete(s.url(&format!("/session/{id}")))
        .query(&s.q())
        .send()
        .await;
    let v = out.map_err(|_| anyhow::anyhow!("OpenCode took too long to suggest"))??;
    Ok(v["parts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|p| p["type"] == "text")
        .filter_map(|p| p["text"].as_str())
        .collect::<Vec<_>>()
        .join("")
        .trim()
        .to_string())
}
