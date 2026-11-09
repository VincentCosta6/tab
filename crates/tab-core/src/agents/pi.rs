//! Pi adapter: one `pi --mode json` process per turn. Each chat is the Pi
//! session with the chat's own id (`--session-id`), so every turn picks up
//! where the last one ended and the person can open it in Pi too.
//!
//! Pi has no approvals: where tab's sandbox is enforced it bounds what Pi
//! runs. tab's MCP server isn't connected yet, since Pi only reads servers
//! from its own config files.

use std::collections::HashSet;
use std::sync::Arc;

use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::AgentHub;
use super::per_turn::{self, Events};
use crate::model::{AgentKind, Session, SessionStatus};
use crate::sandbox::Trust;

fn strict(chosen: bool, trust: Trust) -> bool {
    chosen || trust != Trust::Person
}

pub(super) struct Options<'a> {
    pub strict: bool,
    pub model: Option<&'a str>,
    /// File (or text) appended to Pi's system prompt.
    pub system: &'a std::path::Path,
    pub session: &'a str,
    pub prompt: &'a str,
}

pub(super) fn args(o: &Options) -> Vec<std::ffi::OsString> {
    let mut a: Vec<std::ffi::OsString> = vec!["--mode".into(), "json".into()];
    if o.strict {
        // Only what tab passes: no extensions (MCP is one), skills or prompt
        // templates of the person's, and no project-local config that asks
        // to be trusted.
        for f in [
            "--no-extensions",
            "--no-skills",
            "--no-prompt-templates",
            "--no-approve",
        ] {
            a.push(f.into());
        }
    }
    if let Some(m) = o.model {
        a.push("--model".into());
        a.push(m.into());
    }
    a.push("--append-system-prompt".into());
    a.push(o.system.into());
    a.push("--session-id".into());
    a.push(o.session.into());
    a.push("--".into());
    a.push(o.prompt.into());
    a
}

pub(super) async fn run_turn(
    hub: &Arc<AgentHub>,
    session: &Session,
    prompt: &str,
    system: &str,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    let cfg = hub.config_for(session);
    let exe = super::agent_exe(&cfg, AgentKind::Pi)?;
    let dir = hub.session_dir(&session.id)?;
    let system_file = dir.join("system.md");
    std::fs::write(&system_file, system)?;
    let policy = hub.policy(session);
    let pi_session = session.agent_session_id.as_deref().unwrap_or(&session.id);
    let strict = strict(cfg.strict, policy.trust);
    let mut a = super::Args::default();
    a.args(args(&Options {
        strict,
        model: cfg.model_for(AgentKind::Pi),
        system: &system_file,
        session: pi_session,
        prompt,
    }));
    let mut events = Parser::new(hub.clone(), session.id.clone());
    events.strict = strict;
    per_turn::run(
        hub,
        session,
        per_turn::Turn {
            name: "Pi",
            exe: &exe,
            args: a,
            env: Vec::new(),
            stdin: None,
        },
        &mut events,
        cancel,
    )
    .await
}

/// Turns Pi's JSON events into transcript entries.
pub(super) struct Parser {
    hub: Arc<AgentHub>,
    session_id: String,
    /// Tool calls that create something linkable.
    create_calls: HashSet<String>,
    /// The last assistant message's error, if it ended on one.
    error: Option<String>,
    cost: f64,
    finished: bool,
    /// Strict mode: Pi runs without extensions, so no MCP tool may show up.
    pub(super) strict: bool,
    leaked: Option<String>,
}

impl Parser {
    pub(super) fn new(hub: Arc<AgentHub>, session_id: String) -> Self {
        Self {
            hub,
            session_id,
            create_calls: HashSet::new(),
            error: None,
            cost: 0.0,
            finished: false,
            strict: false,
            leaked: None,
        }
    }

    fn assistant(&mut self, m: &Value) {
        let sid = self.session_id.clone();
        let hub = self.hub.clone();
        for block in m["content"].as_array().into_iter().flatten() {
            match block["type"].as_str() {
                Some("text") => {
                    let text = block["text"].as_str().unwrap_or_default();
                    if !text.trim().is_empty() {
                        hub.post(&sid, "assistant", "text", text, Value::Null);
                    }
                }
                Some("toolCall") => {
                    let id = block["id"].as_str().unwrap_or_default().to_string();
                    let name = block["name"].as_str().unwrap_or("tool");
                    let input = &block["arguments"];
                    if self.strict && name.starts_with("mcp__") {
                        self.leaked = Some(format!("the MCP tool {name}"));
                    }
                    if crate::links::is_creator(name, input) {
                        self.create_calls.insert(id.clone());
                    }
                    hub.post(
                        &sid,
                        "assistant",
                        "tool",
                        &super::summarize_tool(name, input),
                        json!({ "tool": name, "id": id, "input": input }),
                    );
                }
                _ => {}
            }
        }
        let u = &m["usage"];
        let cost = u["cost"]["total"].as_f64().unwrap_or(0.0);
        self.cost += cost;
        let tokens = |k: &str| u[k].as_i64().unwrap_or(0);
        let usage = crate::store::NewUsage {
            purpose: "agent",
            session_id: Some(sid.clone()),
            agent: AgentKind::Pi.as_str().into(),
            model: m["model"]
                .as_str()
                .map(|model| match m["provider"].as_str() {
                    Some(p) => format!("{p}/{model}"),
                    None => model.to_string(),
                }),
            billing: "unknown",
            cost_usd: cost,
            input_tokens: tokens("input") + tokens("cacheRead") + tokens("cacheWrite"),
            output_tokens: tokens("output"),
        };
        if u.is_object()
            && let Err(e) = hub.store.usage_add(&usage, chrono::Utc::now())
        {
            tracing::warn!(error = %e, "couldn't store usage");
        }
        self.error = matches!(m["stopReason"].as_str(), Some("error")).then(|| {
            m["errorMessage"]
                .as_str()
                .unwrap_or("The model returned an error")
                .to_string()
        });
    }

    fn handle_event(&mut self, v: &Value) {
        let sid = self.session_id.clone();
        let hub = self.hub.clone();
        match v["type"].as_str().unwrap_or_default() {
            "session" => {
                if let Some(id) = v["id"].as_str() {
                    hub.update(&sid, None, Some(id));
                }
            }
            "message_update" => {
                let e = &v["assistantMessageEvent"];
                if e["type"] == "text_delta"
                    && let Some(t) = e["delta"].as_str()
                {
                    hub.delta(&sid, t);
                }
            }
            "message_end" if v["message"]["role"] == "assistant" => {
                self.assistant(&v["message"]);
            }
            "tool_execution_end" => {
                let id = v["toolCallId"].as_str().unwrap_or_default();
                let text = v["result"]["content"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|p| p["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                if self.create_calls.remove(id) {
                    hub.attach_created_links(&sid, &text);
                }
                hub.post(
                    &sid,
                    "tool",
                    "tool_result",
                    &text.chars().take(4000).collect::<String>(),
                    json!({ "tool_use_id": id, "is_error": v["isError"] }),
                );
            }
            "auto_retry_start" => {
                if let Some(msg) = v["errorMessage"].as_str() {
                    hub.post(
                        &sid,
                        "system",
                        "status",
                        &format!("Retrying: {msg}"),
                        Value::Null,
                    );
                }
            }
            "agent_end" if v["willRetry"] != true => self.finish(),
            "agent_settled" => self.finish(),
            _ => {}
        }
    }

    fn finish(&mut self) {
        if std::mem::replace(&mut self.finished, true) {
            return;
        }
        let sid = &self.session_id;
        if let Some(e) = &self.error {
            self.hub.post(sid, "system", "error", e, Value::Null);
        }
        self.hub.post(
            sid,
            "system",
            "result",
            "",
            json!({ "cost_usd": self.cost }),
        );
        self.hub.update(
            sid,
            Some(if self.error.is_some() {
                SessionStatus::Failed
            } else {
                SessionStatus::Idle
            }),
            None,
        );
    }
}

impl Events for Parser {
    fn handle(&mut self, v: &Value) {
        self.handle_event(v);
    }
    fn finished(&self) -> bool {
        self.finished
    }
    fn leaked(&self) -> Option<String> {
        self.leaked.clone()
    }
}

/// One answer through Pi for background helpers: no tools, no saved
/// session, none of the person's extensions or context files.
pub(super) async fn complete(
    hub: &Arc<AgentHub>,
    system: &str,
    prompt: &str,
    model: Option<&str>,
) -> anyhow::Result<String> {
    let exe = super::agent_exe(&hub.config(), AgentKind::Pi)?;
    let mut a: Vec<std::ffi::OsString> = [
        "--print",
        "--no-session",
        "--no-tools",
        "--no-extensions",
        "--no-skills",
        "--no-prompt-templates",
        "--no-context-files",
        "--no-approve",
        "--system-prompt",
    ]
    .map(Into::into)
    .into();
    a.push(system.into());
    if let Some(m) = model {
        a.push("--model".into());
        a.push(m.into());
    }
    a.push("--".into());
    a.push(prompt.into());
    let out = per_turn::capture(&exe, &a, None, std::time::Duration::from_secs(90)).await?;
    Ok(out.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AgentConfig;
    use crate::store::Store;
    use chrono::Utc;

    #[test]
    fn a_turn_names_its_session_and_ends_options_before_the_prompt() {
        let a = args(&Options {
            strict: true,
            model: Some("anthropic/claude-sonnet-5-5"),
            system: std::path::Path::new("/tmp/system.md"),
            session: "abc-123",
            prompt: "--looks like a flag",
        });
        let a: Vec<String> = a.iter().map(|s| s.to_string_lossy().into()).collect();
        assert_eq!(&a[..2], ["--mode", "json"]);
        assert!(a.contains(&"--no-extensions".to_string()));
        assert!(a.windows(2).any(|w| w == ["--session-id", "abc-123"]));
        assert!(
            a.windows(2)
                .any(|w| w == ["--append-system-prompt", "/tmp/system.md"])
        );
        assert_eq!(&a[a.len() - 2..], ["--", "--looks like a flag"]);
    }

    #[test]
    fn not_strict_keeps_the_persons_setup() {
        let a = args(&Options {
            strict: false,
            model: None,
            system: std::path::Path::new("s.md"),
            session: "x",
            prompt: "hi",
        });
        assert!(!a.iter().any(|x| x == "--no-extensions" || x == "--model"));
    }

    fn hub_and_session(name: &str) -> (Arc<AgentHub>, Arc<Store>, Session) {
        let store = Arc::new(Store::in_memory().unwrap());
        let (tx, _rx) = tokio::sync::broadcast::channel(64);
        let hub = AgentHub::new(
            store.clone(),
            tx,
            AgentConfig::default(),
            std::env::temp_dir().join(name),
        );
        let s = store
            .session_create(None, AgentKind::Pi, "t", ".", Utc::now())
            .unwrap();
        (hub, store, s)
    }

    #[tokio::test]
    async fn parses_a_json_mode_run() {
        let (hub, store, s) = hub_and_session("tab-pi-test");
        let mut p = Parser::new(hub, s.id.clone());
        let lines = [
            r#"{"type":"session","version":3,"id":"pi-session-1","timestamp":"2026-11-08T00:00:00.000Z","cwd":"/w"}"#,
            r#"{"type":"agent_start"}"#,
            r#"{"type":"turn_start"}"#,
            r#"{"type":"message_end","message":{"role":"user","content":"list files","timestamp":1}}"#,
            r#"{"type":"message_update","usage":{},"assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"Look"}}"#,
            r#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"Looking."},{"type":"toolCall","id":"call_1","name":"bash","arguments":{"command":"ls"}}],"provider":"anthropic","model":"claude-sonnet-5-5","usage":{"input":10,"output":5,"cacheRead":100,"cacheWrite":0,"totalTokens":115,"cost":{"total":0.002}},"stopReason":"toolUse","timestamp":2}}"#,
            r#"{"type":"tool_execution_start","toolCallId":"call_1","toolName":"bash","args":{"command":"ls"}}"#,
            r#"{"type":"tool_execution_end","toolCallId":"call_1","toolName":"bash","result":{"content":[{"type":"text","text":"README.md"}],"details":{}},"isError":false}"#,
            r#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"One file."}],"provider":"anthropic","model":"claude-sonnet-5-5","usage":{"input":12,"output":3,"cacheRead":0,"cacheWrite":0,"totalTokens":15,"cost":{"total":0.001}},"stopReason":"stop","timestamp":3}}"#,
            r#"{"type":"agent_end","messages":[],"willRetry":false}"#,
            r#"{"type":"agent_settled","aborted":false}"#,
        ];
        for l in lines {
            p.handle(&serde_json::from_str(l).unwrap());
        }
        assert!(p.finished());
        let got = store.session_get(&s.id).unwrap().unwrap();
        assert_eq!(got.agent_session_id.as_deref(), Some("pi-session-1"));
        assert_eq!(got.status, SessionStatus::Idle);
        let m = store.messages(&s.id).unwrap();
        let kinds: Vec<_> = m.iter().map(|m| m.kind.as_str()).collect();
        assert_eq!(kinds, ["text", "tool", "tool_result", "text", "result"]);
        assert_eq!(m[1].content, "bash: ls");
        assert_eq!(m[2].content, "README.md");
        // One result, even with both end events.
        assert!((m[4].meta["cost_usd"].as_f64().unwrap() - 0.003).abs() < 1e-9);
    }

    #[tokio::test]
    async fn a_model_error_fails_the_turn() {
        let (hub, store, s) = hub_and_session("tab-pi-test-2");
        let mut p = Parser::new(hub, s.id.clone());
        p.handle(&json!({ "type": "message_end", "message": { "role": "assistant", "content": [], "usage": {}, "stopReason": "error", "errorMessage": "No API key for anthropic" } }));
        p.handle(&json!({ "type": "agent_end", "messages": [], "willRetry": false }));
        assert_eq!(
            store.session_get(&s.id).unwrap().unwrap().status,
            SessionStatus::Failed
        );
        assert!(
            store
                .messages(&s.id)
                .unwrap()
                .iter()
                .any(|m| m.kind == "error" && m.content.contains("No API key"))
        );
    }
}
