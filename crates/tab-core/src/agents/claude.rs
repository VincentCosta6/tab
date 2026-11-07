//! Claude Code adapter: one `claude -p` process per turn, resumed by session id,
//! read as `stream-json`.

use std::collections::HashSet;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio_util::sync::CancellationToken;

use super::{AgentHub, Steer, default_command, resolve};
use crate::model::{AgentKind, Session, SessionStatus};
use crate::sandbox::Trust;

/// Claude Code permission modes tab passes through. `bypassPermissions` is
/// deliberately not offered: approvals belong to the person.
const MODES: &[&str] = &["default", "acceptEdits", "plan", "auto", "dontAsk"];

/// Tool calls in flight, by id. Claude Code reads a steering message only
/// between tool calls, so "Send now" interrupts while one is running.
type OpenTools = Arc<Mutex<HashSet<String>>>;

/// The stdin line for a steering command, if it needs one: a message is
/// queued as a user turn; an interrupt (Esc in the terminal) is sent only
/// while a tool runs, since otherwise the message is read at the next step.
fn steer_line(steer: Steer, tools_running: bool, n: u64) -> Option<Value> {
    match steer {
        Steer::Message(text) => {
            Some(json!({ "type": "user", "message": { "role": "user", "content": text } }))
        }
        Steer::Interrupt if tools_running => Some(json!({
            "type": "control_request",
            "request_id": format!("tab-interrupt-{n}"),
            "request": { "subtype": "interrupt" },
        })),
        Steer::Interrupt => None,
    }
}

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

    let policy = hub.policy(session);
    let settings_file = dir.join("settings.json");
    std::fs::write(
        &settings_file,
        serde_json::to_vec_pretty(&crate::sandbox::current().claude_settings(&policy))?,
    )?;
    let strict = strict(cfg.strict, policy.trust);
    let mut cmd = super::Args::default();
    cmd.arg("--settings").arg(&settings_file);
    cmd.args(isolation_args(cfg.strict, policy.trust));
    cmd.args([
        "-p",
        "--input-format",
        "stream-json",
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
    // Images the person attached live here; reading them needs no prompt.
    let attachments = hub.attachments_dir();
    if attachments.is_dir() {
        cmd.arg("--add-dir").arg(&attachments);
    }
    if let Some(mcp) = hub.mcp() {
        let mcp_file = dir.join("mcp.json");
        let config = json!({
            "mcpServers": {
                "tab": {
                    "type": "http",
                    "url": mcp.url,
                    "headers": {
                        "Authorization": format!("Bearer {}", crate::mcp::chat_token(&mcp.token, &session.id)),
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
    let mut prepared = crate::sandbox::current()
        .command(&exe, &cmd.0, std::path::Path::new(&session.cwd), &policy)
        .await?;
    hub.add_shims(session, &mut prepared.cmd);
    if strict {
        // claude.ai connectors too, should a version load them despite
        // `--strict-mcp-config`.
        prepared.cmd.env("ENABLE_CLAUDEAI_MCP_SERVERS", "false");
    }
    prepared
        .cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    tracing::debug!(session = %session.id, mode, strict, resume = ?session.agent_session_id, sandboxed = prepared.enforced, "starting Claude Code turn");
    let mut child = prepared
        .spawn()
        .map_err(|e| anyhow::anyhow!("starting Claude Code ({}) failed: {e}", exe.display()))?;
    // stdin stays open so the person can steer mid-turn; it closes when the
    // turn's result arrives (see `steering_close`) and the queue drains.
    let mut stdin = child.stdin.take().expect("piped");
    let first = json!({ "type": "user", "message": { "role": "user", "content": prompt } });
    stdin.write_all(format!("{first}\n").as_bytes()).await?;
    let mut steer_rx = hub.steering_open(&session.id);
    let open_tools = OpenTools::default();
    let tools = open_tools.clone();
    tokio::spawn(async move {
        let mut n = 0;
        while let Some(steer) = steer_rx.recv().await {
            n += 1;
            let running = !tools.lock().unwrap().is_empty();
            let Some(line) = steer_line(steer, running, n) else {
                continue;
            };
            if stdin
                .write_all(format!("{line}\n").as_bytes())
                .await
                .is_err()
            {
                break;
            }
        }
    });

    let stdout = child.stdout.take().expect("piped");
    let mut stderr = child.stderr.take().expect("piped");
    let err_task = tokio::spawn(async move {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s).await;
        s
    });

    let mut lines = BufReader::new(stdout).lines();
    let mut parser = Parser::new(hub.clone(), session.id.clone());
    parser.open_tools = open_tools;
    parser.strict = strict;
    // Rough size of what tab adds to the system prompt (the item and its context).
    parser.tab_context_tokens = (system.len() / 4) as i64;
    // After the result the CLI normally exits right away. If it stays up,
    // it's waiting on background work it started, so the chat is monitoring.
    let mut monitoring = false;
    loop {
        let settle = async {
            if parser.finished && !parser.failed && !monitoring {
                tokio::time::sleep(std::time::Duration::from_millis(1500)).await
            } else {
                std::future::pending::<()>().await
            }
        };
        tokio::select! {
            _ = settle => {
                monitoring = true;
                hub.update(&session.id, Some(SessionStatus::Monitoring), None);
            }
            line = lines.next_line() => match line? {
                Some(l) if !l.trim().is_empty() => match serde_json::from_str::<Value>(&l) {
                    Ok(v) => {
                        parser.handle(&v);
                        if !parser.leaked.is_empty() {
                            child.kill_tree();
                            anyhow::bail!(
                                "Strict mode stopped this turn: Claude Code loaded {}, which isn't one of tab's connections.",
                                parser.leaked.join(", ")
                            );
                        }
                        // Background work woke the agent up: it's working again.
                        if !parser.finished {
                            monitoring = false;
                        }
                    }
                    Err(_) => tracing::debug!(line = %l, "non-json output from claude"),
                },
                Some(_) => {}
                None => break,
            },
            _ = cancel.cancelled() => {
                child.kill_tree();
                return Ok(());
            }
        }
    }
    if monitoring {
        hub.update(&session.id, Some(SessionStatus::Idle), None);
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
    /// Ids of calls that create something linkable, so their output can be attached as Context.
    create_calls: HashSet<String>,
    finished: bool,
    /// The last result was an error.
    failed: bool,
    billing: &'static str,
    /// Usage of the first and latest model call this turn, for the context meter.
    first_call: Option<Value>,
    last_call: Option<Value>,
    tab_context_tokens: i64,
    /// Shared with the steering task, which interrupts only while one runs.
    open_tools: OpenTools,
    /// The person pressed "Send now": the next result only closes the
    /// interrupted part, and the turn goes on with their queued message.
    interrupted: bool,
    /// Strict mode: only tab's MCP server may load.
    strict: bool,
    /// What loaded anyway, from the init line. Non-empty stops the turn.
    leaked: Vec<String>,
}

/// Whether a chat runs strict: when the person chose it, and always for an
/// agent working on someone else's code.
fn strict(chosen: bool, trust: Trust) -> bool {
    chosen || trust != Trust::Person
}

/// What keeps the person's own Claude Code setup out of a chat.
///
/// Strict: only `--mcp-config` servers (tab's), and only the repo's settings
/// (`project`, `local`): no user MCP servers, claude.ai connectors, user
/// hooks, plugins, allow rules or `~/.claude/CLAUDE.md`. Code nobody here
/// wrote (a pull request's checkout) gets no settings at all, since its
/// `.claude/settings.json` hooks would run as soon as the chat starts.
fn isolation_args(chosen: bool, trust: Trust) -> Vec<&'static str> {
    match (strict(chosen, trust), trust) {
        (false, _) => Vec::new(),
        (true, Trust::Person) => vec!["--strict-mcp-config", "--setting-sources", "project,local"],
        (true, _) => vec!["--strict-mcp-config", "--setting-sources", ""],
    }
}

/// MCP servers and account plugins in Claude Code's init line that strict
/// mode doesn't allow: anything but tab's own server.
fn unexpected(init: &Value) -> Vec<String> {
    let servers = init["mcp_servers"].as_array().into_iter().flatten();
    let plugins = init["plugins"].as_array().into_iter().flatten();
    servers
        .filter_map(|s| s["name"].as_str())
        .filter(|n| *n != "tab")
        .map(|n| format!("MCP server \"{n}\""))
        .chain(
            plugins
                .filter(|p| p["source"].as_str().is_some_and(|s| s.ends_with("@synced")))
                .filter_map(|p| p["name"].as_str())
                .map(|n| format!("plugin \"{n}\"")),
        )
        .collect()
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
            create_calls: HashSet::new(),
            finished: false,
            failed: false,
            billing: "unknown",
            first_call: None,
            last_call: None,
            tab_context_tokens: 0,
            open_tools: OpenTools::default(),
            interrupted: false,
            strict: false,
            leaked: Vec::new(),
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
        let kind = v["type"].as_str().unwrap_or_default();
        // Output after the result means background work (a finished
        // background command, a watcher firing) started a new turn.
        if self.finished && matches!(kind, "assistant" | "stream_event") {
            self.finished = false;
            hub.update(&sid, Some(SessionStatus::Working), None);
        }
        match kind {
            "system" if v["subtype"] == "init" => {
                if self.strict {
                    self.leaked = unexpected(v);
                }
                self.billing = crate::usage::billing_from_init(v);
                if let Some(cmds) = v["slash_commands"].as_array() {
                    let names: Vec<String> = cmds
                        .iter()
                        .filter_map(|c| c.as_str().map(str::to_string))
                        .collect();
                    hub.save_slash_commands(&names);
                }
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
                                self.open_tools.lock().unwrap().insert(id.clone());
                                let name = block["name"].as_str().unwrap_or("tool");
                                if crate::links::is_creator(name, &block["input"]) {
                                    self.create_calls.insert(id.clone());
                                }
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
                    if block["type"] == "text"
                        && block["text"]
                            .as_str()
                            .is_some_and(|t| t.starts_with("[Request interrupted by user"))
                    {
                        self.interrupted = true;
                    }
                    if block["type"] == "tool_result" {
                        if let Some(id) = block["tool_use_id"].as_str() {
                            self.open_tools.lock().unwrap().remove(id);
                        }
                        let text = match &block["content"] {
                            Value::String(s) => s.clone(),
                            Value::Array(parts) => parts
                                .iter()
                                .filter_map(|p| p["text"].as_str())
                                .collect::<Vec<_>>()
                                .join("\n"),
                            _ => String::new(),
                        };
                        if block["tool_use_id"]
                            .as_str()
                            .is_some_and(|i| self.create_calls.remove(i))
                        {
                            hub.attach_created_links(&sid, &text);
                        }
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
            "result" if std::mem::take(&mut self.interrupted) => {
                // Interrupted to read steering that is already queued:
                // count the cost and keep going.
                self.open_tools.lock().unwrap().clear();
                crate::usage::record_claude_result(
                    &hub.store,
                    v,
                    "agent",
                    Some(&sid),
                    self.billing,
                );
            }
            "result" => {
                self.finished = true;
                self.open_tools.lock().unwrap().clear();
                hub.steering_close(&sid);
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
                self.failed = failed;
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

    #[tokio::test]
    async fn output_after_the_result_marks_the_session_working_again() {
        let store = Arc::new(Store::in_memory().unwrap());
        let (tx, _rx) = tokio::sync::broadcast::channel(64);
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
        p.handle(
            &serde_json::from_str(r#"{"type":"result","session_id":"abc","is_error":false}"#)
                .unwrap(),
        );
        assert!(p.finished && !p.failed);
        // A background command finished and the agent picked back up.
        p.handle(&serde_json::from_str(r#"{"type":"assistant","message":{"id":"m3","content":[{"type":"text","text":"Build done"}]}}"#).unwrap());
        assert!(!p.finished);
        let s = store.session_get(&s.id).unwrap().unwrap();
        assert_eq!(s.status, SessionStatus::Working);
    }

    #[test]
    fn messages_queue_and_send_now_interrupts_only_a_running_tool() {
        let msg = steer_line(Steer::Message("hi".into()), true, 1).unwrap();
        assert_eq!(
            (msg["type"].as_str(), msg["message"]["content"].as_str()),
            (Some("user"), Some("hi"))
        );
        let stop = steer_line(Steer::Interrupt, true, 7).unwrap();
        assert_eq!(stop["request"]["subtype"], "interrupt");
        assert_eq!(stop["request_id"], "tab-interrupt-7");
        // Between tool calls the queued message is read anyway.
        assert_eq!(steer_line(Steer::Interrupt, false, 8), None);
    }

    #[tokio::test]
    async fn an_interrupt_to_steer_does_not_end_the_turn() {
        let store = Arc::new(Store::in_memory().unwrap());
        let (tx, _rx) = tokio::sync::broadcast::channel(64);
        let hub = AgentHub::new(
            store.clone(),
            tx,
            AgentConfig::default(),
            std::env::temp_dir(),
        );
        let s = store
            .session_create(None, AgentKind::ClaudeCode, "t", ".", Utc::now())
            .unwrap();
        hub.steering_open(&s.id);
        let mut p = Parser::new(hub.clone(), s.id.clone());
        let feed = |p: &mut Parser, line: &str| p.handle(&serde_json::from_str(line).unwrap());
        feed(
            &mut p,
            r#"{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"gh pr checks --watch"}}]}}"#,
        );
        assert!(p.open_tools.lock().unwrap().contains("t1"));
        // What Claude Code prints after an interrupt mid-tool.
        feed(
            &mut p,
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"The user doesn't want to proceed with this tool use.","is_error":true}]}}"#,
        );
        feed(
            &mut p,
            r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"[Request interrupted by user for tool use]"}]}}"#,
        );
        assert!(p.open_tools.lock().unwrap().is_empty());
        feed(
            &mut p,
            r#"{"type":"result","session_id":"abc","is_error":false,"stop_reason":"tool_use"}"#,
        );
        assert!(!p.finished, "the turn goes on with the steering message");
        assert!(hub.can_steer(&s.id), "steering stays open");
        let kinds: Vec<_> = store
            .messages(&s.id)
            .unwrap()
            .into_iter()
            .map(|m| m.kind)
            .collect();
        assert!(!kinds.contains(&"result".to_string()));
        // The steering message is answered, and that result ends the turn.
        feed(
            &mut p,
            r#"{"type":"assistant","message":{"id":"m2","content":[{"type":"text","text":"Skipping the wait."}]}}"#,
        );
        feed(
            &mut p,
            r#"{"type":"result","session_id":"abc","is_error":false,"stop_reason":"end_turn"}"#,
        );
        assert!(p.finished);
        assert!(!hub.can_steer(&s.id));
    }

    #[test]
    fn strict_chats_load_only_the_repos_settings() {
        assert_eq!(
            isolation_args(true, Trust::Person),
            ["--strict-mcp-config", "--setting-sources", "project,local"]
        );
        assert!(isolation_args(false, Trust::Person).is_empty());
    }

    #[test]
    fn review_chats_are_always_strict_and_load_no_settings() {
        for chosen in [true, false] {
            for trust in [Trust::Agent, Trust::Untrusted] {
                assert_eq!(
                    isolation_args(chosen, trust),
                    ["--strict-mcp-config", "--setting-sources", ""]
                );
            }
        }
    }

    #[test]
    fn strict_flags_anything_but_tab_in_the_init_line() {
        let clean = json!({
            "mcp_servers": [{ "name": "tab", "status": "connected" }],
            "plugins": [{ "name": "cc-plugin-telemetry", "source": "cc-plugin-telemetry@builtin" }],
        });
        assert!(unexpected(&clean).is_empty());
        let leaky = json!({
            "mcp_servers": [{ "name": "tab" }, { "name": "claude.ai Health", "status": "connected" }],
            "plugins": [{ "name": "cloudflare", "source": "cloudflare@synced" }],
        });
        assert_eq!(
            unexpected(&leaky),
            ["MCP server \"claude.ai Health\"", "plugin \"cloudflare\""]
        );
    }

    #[tokio::test]
    async fn a_strict_parser_records_what_leaked() {
        let store = Arc::new(Store::in_memory().unwrap());
        let (tx, _rx) = tokio::sync::broadcast::channel(8);
        let hub = AgentHub::new(
            store.clone(),
            tx,
            AgentConfig::default(),
            std::env::temp_dir(),
        );
        let s = store
            .session_create(None, AgentKind::ClaudeCode, "t", ".", Utc::now())
            .unwrap();
        let init =
            json!({ "type": "system", "subtype": "init", "mcp_servers": [{ "name": "health" }] });
        let mut loose = Parser::new(hub.clone(), s.id.clone());
        loose.handle(&init);
        assert!(loose.leaked.is_empty());
        let mut p = Parser::new(hub, s.id.clone());
        p.strict = true;
        p.handle(&init);
        assert_eq!(p.leaked, ["MCP server \"health\""]);
    }
}
