//! Claude Code adapter: one `claude -p` process per turn, resumed by session id,
//! read as `stream-json`.

use std::collections::HashSet;
use std::process::Stdio;
use std::sync::Arc;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio_util::sync::CancellationToken;

use super::{AgentHub, default_command, resolve};
use crate::model::{AgentKind, Session, SessionStatus};

/// Claude Code permission modes tab passes through. `bypassPermissions` is
/// deliberately not offered: approvals belong to the person.
const MODES: &[&str] = &["default", "acceptEdits", "plan", "auto", "dontAsk"];

pub(super) async fn run_turn(
    hub: &Arc<AgentHub>,
    session: &Session,
    prompt: &str,
    system: &str,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    let cfg = hub.config();
    let exe = resolve(
        cfg.command
            .as_deref()
            .filter(|c| !c.trim().is_empty() && cfg.kind == Some(AgentKind::ClaudeCode))
            .unwrap_or(default_command(AgentKind::ClaudeCode)),
    )?;
    let dir = hub.session_dir(&session.id)?;
    let system_file = dir.join("system.md");
    std::fs::write(&system_file, system)?;

    let mut cmd = super::command(&exe);
    cmd.args([
        "-p",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
    ]);
    let mode = if MODES.contains(&cfg.permission_mode.as_str()) {
        cfg.permission_mode.as_str()
    } else {
        "default"
    };
    cmd.args(["--permission-mode", mode]);
    cmd.arg("--append-system-prompt-file").arg(&system_file);
    if let Some(mcp) = hub.mcp() {
        let mcp_file = dir.join("mcp.json");
        let config = json!({
            "mcpServers": {
                "tab": {
                    "type": "http",
                    "url": mcp.url,
                    "headers": {
                        "Authorization": format!("Bearer {}", mcp.token),
                        "X-Tab-Session": session.id,
                    }
                }
            }
        });
        std::fs::write(&mcp_file, serde_json::to_vec_pretty(&config)?)?;
        cmd.arg("--mcp-config").arg(&mcp_file);
        // Approvals come to tab; tab's own tools need none.
        cmd.args(["--permission-prompt-tool", "mcp__tab__approve"]);
        cmd.args(["--allowedTools", "mcp__tab"]);
    }
    if let Some(m) = cfg.model.as_deref().filter(|m| !m.trim().is_empty()) {
        cmd.args(["--model", m]);
    }
    if let Some(cap) = cfg.budget.per_turn_usd.filter(|c| *c > 0.0) {
        cmd.args(["--max-budget-usd", &format!("{cap:.2}")]);
    }
    if let Some(id) = &session.agent_session_id {
        cmd.args(["--resume", id]);
    }
    cmd.current_dir(&session.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|e| anyhow::anyhow!("starting Claude Code ({}) failed: {e}", exe.display()))?;
    let mut stdin = child.stdin.take().expect("piped");
    stdin.write_all(prompt.as_bytes()).await?;
    drop(stdin);

    let stdout = child.stdout.take().expect("piped");
    let mut stderr = child.stderr.take().expect("piped");
    let err_task = tokio::spawn(async move {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s).await;
        s
    });

    let mut lines = BufReader::new(stdout).lines();
    let mut parser = Parser::new(hub.clone(), session.id.clone());
    // Rough size of what tab adds to the system prompt (the item and its context).
    parser.tab_context_tokens = (system.len() / 4) as i64;
    loop {
        tokio::select! {
            line = lines.next_line() => match line? {
                Some(l) if !l.trim().is_empty() => match serde_json::from_str::<Value>(&l) {
                    Ok(v) => parser.handle(&v),
                    Err(_) => tracing::debug!(line = %l, "non-json output from claude"),
                },
                Some(_) => {}
                None => break,
            },
            _ = cancel.cancelled() => {
                let _ = child.kill().await;
                return Ok(());
            }
        }
    }
    let status = child.wait().await?;
    let stderr = err_task.await.unwrap_or_default();
    if !parser.finished {
        let tail: String = stderr
            .lines()
            .rev()
            .take(8)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n");
        anyhow::bail!(
            "Claude Code exited ({status}) before finishing.{}",
            if tail.is_empty() {
                String::new()
            } else {
                format!("\n{tail}")
            }
        );
    }
    Ok(())
}

/// Turns stream-json lines into transcript entries.
struct Parser {
    hub: Arc<AgentHub>,
    session_id: String,
    seen: HashSet<String>,
    finished: bool,
    billing: &'static str,
    /// Usage of the first and latest model call this turn, for the context meter.
    first_call: Option<Value>,
    last_call: Option<Value>,
    tab_context_tokens: i64,
}

/// Everything a model call read: new input plus cache writes and reads.
fn prompt_tokens(u: &Value) -> i64 {
    [
        "input_tokens",
        "cache_creation_input_tokens",
        "cache_read_input_tokens",
    ]
    .iter()
    .map(|k| u[*k].as_i64().unwrap_or(0))
    .sum()
}

impl Parser {
    fn new(hub: Arc<AgentHub>, session_id: String) -> Self {
        Self {
            hub,
            session_id,
            seen: HashSet::new(),
            finished: false,
            billing: "unknown",
            first_call: None,
            last_call: None,
            tab_context_tokens: 0,
        }
    }

    /// How full the context window is after this turn, and what fills it.
    fn context(&self, result: &Value) -> Value {
        let Some(last) = &self.last_call else {
            return Value::Null;
        };
        let (model, window) = result["modelUsage"]
            .as_object()
            .and_then(|m| {
                m.iter()
                    .max_by_key(|(_, u)| u["outputTokens"].as_i64().unwrap_or(0))
                    .map(|(k, u)| (k.clone(), u["contextWindow"].as_i64()))
            })
            .unwrap_or_default();
        json!({
            "model": model,
            "window": window.unwrap_or(200_000),
            // The next turn starts from everything the last call read plus what it wrote.
            "used": prompt_tokens(last) + last["output_tokens"].as_i64().unwrap_or(0),
            "first_call": self.first_call.as_ref().map(prompt_tokens),
            "cache_read": last["cache_read_input_tokens"],
            "cache_write": last["cache_creation_input_tokens"],
            "fresh_input": last["input_tokens"],
            "output": last["output_tokens"],
            "tab_context": self.tab_context_tokens,
        })
    }

    fn once(&mut self, key: String) -> bool {
        self.seen.insert(key)
    }

    fn handle(&mut self, v: &Value) {
        let sid = self.session_id.clone();
        let hub = self.hub.clone();
        match v["type"].as_str().unwrap_or_default() {
            "system" if v["subtype"] == "init" => {
                self.billing = crate::usage::billing_from_init(v);
                if let Some(id) = v["session_id"].as_str() {
                    hub.update(&sid, None, Some(id));
                }
            }
            "stream_event" => {
                let ev = &v["event"];
                if ev["type"] == "content_block_delta"
                    && ev["delta"]["type"] == "text_delta"
                    && let Some(t) = ev["delta"]["text"].as_str()
                {
                    hub.delta(&sid, t);
                }
            }
            "assistant" => {
                let msg_id = v["message"]["id"].as_str().unwrap_or_default().to_string();
                let usage = &v["message"]["usage"];
                if usage.is_object() && prompt_tokens(usage) > 0 {
                    if self.first_call.is_none() {
                        self.first_call = Some(usage.clone());
                    }
                    self.last_call = Some(usage.clone());
                }
                for (i, block) in v["message"]["content"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .enumerate()
                {
                    match block["type"].as_str() {
                        Some("text") => {
                            let text = block["text"].as_str().unwrap_or_default();
                            if !text.trim().is_empty() && self.once(format!("{msg_id}:{i}:{text}"))
                            {
                                hub.post(&sid, "assistant", "text", text, Value::Null);
                            }
                        }
                        Some("tool_use") => {
                            let id = block["id"].as_str().unwrap_or_default().to_string();
                            if self.once(format!("tool:{id}")) {
                                let name = block["name"].as_str().unwrap_or("tool");
                                hub.post(
                                    &sid,
                                    "assistant",
                                    "tool",
                                    &super::summarize_tool(name, &block["input"]),
                                    json!({ "tool": name, "id": id, "input": block["input"] }),
                                );
                            }
                        }
                        _ => {}
                    }
                }
            }
            "user" => {
                for block in v["message"]["content"].as_array().into_iter().flatten() {
                    if block["type"] == "tool_result" {
                        let text = match &block["content"] {
                            Value::String(s) => s.clone(),
                            Value::Array(parts) => parts
                                .iter()
                                .filter_map(|p| p["text"].as_str())
                                .collect::<Vec<_>>()
                                .join("\n"),
                            _ => String::new(),
                        };
                        hub.post(
                            &sid,
                            "tool",
                            "tool_result",
                            &text.chars().take(4000).collect::<String>(),
                            json!({
                                "tool_use_id": block["tool_use_id"],
                                "is_error": block["is_error"],
                            }),
                        );
                    }
                }
            }
            "rate_limit_event" => {
                let info = &v["rate_limit_info"];
                crate::usage::record_rate_limit(&hub.store, info);
                if info["status"]
                    .as_str()
                    .is_some_and(|s| !s.starts_with("allowed"))
                {
                    hub.post(
                        &sid,
                        "system",
                        "status",
                        &format!(
                            "Claude usage limit: {} ({})",
                            info["status"].as_str().unwrap_or_default(),
                            info["rateLimitType"].as_str().unwrap_or("limit")
                        ),
                        info.clone(),
                    );
                }
            }
            "result" => {
                self.finished = true;
                crate::usage::record_claude_result(
                    &hub.store,
                    v,
                    "agent",
                    Some(&sid),
                    self.billing,
                );
                if let Some(id) = v["session_id"].as_str() {
                    hub.update(&sid, None, Some(id));
                }
                let failed = v["is_error"].as_bool().unwrap_or(false);
                if failed {
                    let msg = v["result"]
                        .as_str()
                        .or(v["error"].as_str())
                        .unwrap_or("The turn failed");
                    hub.post(&sid, "system", "error", msg, Value::Null);
                }
                hub.post(
                    &sid,
                    "system",
                    "result",
                    "",
                    json!({
                        "cost_usd": v["total_cost_usd"],
                        "duration_ms": v["duration_ms"],
                        "turns": v["num_turns"],
                        "stop_reason": v["stop_reason"],
                        "context": self.context(v),
                    }),
                );
                hub.update(
                    &sid,
                    Some(if failed {
                        SessionStatus::Failed
                    } else {
                        SessionStatus::Idle
                    }),
                    None,
                );
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AgentConfig;
    use crate::engine::EngineEvent;
    use crate::store::Store;
    use chrono::Utc;

    #[tokio::test]
    async fn parses_a_stream_json_turn() {
        let store = Arc::new(Store::in_memory().unwrap());
        let (tx, mut rx) = tokio::sync::broadcast::channel(64);
        let hub = AgentHub::new(
            store.clone(),
            tx,
            AgentConfig::default(),
            std::env::temp_dir(),
        );
        let s = store
            .session_create(None, AgentKind::ClaudeCode, "t", ".", Utc::now())
            .unwrap();
        let mut p = Parser::new(hub, s.id.clone());
        for line in [
            r#"{"type":"system","subtype":"init","session_id":"abc"}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"Hel"}}}"#,
            r#"{"type":"assistant","message":{"id":"m1","content":[{"type":"text","text":"Hello"}]}}"#,
            r#"{"type":"assistant","message":{"id":"m1","content":[{"type":"text","text":"Hello"}]}}"#,
            r#"{"type":"assistant","message":{"id":"m2","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"ls"}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"a.txt"}]}}"#,
            r#"{"type":"result","session_id":"abc","is_error":false,"total_cost_usd":0.01}"#,
        ] {
            p.handle(&serde_json::from_str(line).unwrap());
        }
        assert!(p.finished);
        let msgs = store.messages(&s.id).unwrap();
        let kinds: Vec<_> = msgs.iter().map(|m| m.kind.as_str()).collect();
        assert_eq!(kinds, ["text", "tool", "tool_result", "result"]);
        assert_eq!(msgs[1].content, "Bash: ls");
        let s = store.session_get(&s.id).unwrap().unwrap();
        assert_eq!(
            (s.agent_session_id.as_deref(), s.status),
            (Some("abc"), SessionStatus::Idle)
        );
        let mut deltas = 0;
        while let Ok(e) = rx.try_recv() {
            if matches!(e, EngineEvent::SessionDelta { .. }) {
                deltas += 1;
            }
        }
        assert_eq!(deltas, 1);
    }
}
