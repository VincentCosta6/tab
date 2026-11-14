//! Codex adapter: one `codex exec --json` process per turn, resumed by
//! thread id.
//!
//! Codex can't hand its approvals to tab from `exec`, so it never asks:
//! where tab's sandbox is enforced, that sandbox is the boundary (Codex's
//! own can't run inside it); elsewhere Codex's `workspace-write` sandbox
//! keeps edits to the work folder.

use std::collections::HashSet;
use std::sync::Arc;

use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::AgentHub;
use super::per_turn::{self, Events};
use crate::model::{AgentKind, Session, SessionStatus};
use crate::sandbox::Trust;

/// Past this, the instructions go in the prompt instead of on the command
/// line (Windows caps a command line at 32K characters).
const MAX_ARG_INSTRUCTIONS: usize = 20_000;

/// A string as TOML spells it, for `-c key=value`. JSON's escapes are a
/// subset of TOML's basic strings.
fn toml_str(s: &str) -> String {
    Value::String(s.into()).to_string()
}

/// Whether a chat runs strict: when the person chose it, and always for an
/// agent working on someone else's code.
fn strict(chosen: bool, trust: Trust) -> bool {
    chosen || trust != Trust::Person
}

pub(super) struct Options<'a> {
    pub strict: bool,
    /// tab's sandbox already bounds the process.
    pub enforced: bool,
    pub model: Option<&'a str>,
    pub system: &'a str,
    /// tab's MCP server: its url and this chat's session id. The token goes
    /// in `TAB_MCP_TOKEN`.
    pub mcp: Option<(&'a str, &'a str)>,
    /// Remote MCP servers through tab, as `(name, url)`; same token.
    pub remote: &'a [(String, String)],
    /// tab's own server goes in too (not on a cloud runner, which fronts
    /// only the connections the person allowed there).
    pub own: bool,
    pub resume: Option<&'a str>,
}

/// `codex exec` arguments for a turn whose prompt comes on stdin.
pub(super) fn args(o: &Options) -> Vec<String> {
    let mut a: Vec<String> = ["exec", "--json", "--skip-git-repo-check"]
        .map(String::from)
        .into();
    if o.strict {
        // No `~/.codex/config.toml` (its MCP servers, profiles, hooks) and
        // no execpolicy rules. Sign-in still comes from `CODEX_HOME`.
        a.extend(["--ignore-user-config", "--ignore-rules"].map(String::from));
    }
    let sandbox = if o.enforced {
        "danger-full-access"
    } else {
        "workspace-write"
    };
    a.extend(["--sandbox".into(), sandbox.into()]);
    let mut config = vec![
        "approval_policy=\"never\"".to_string(),
        // Chats reach the network, like every other harness's.
        "sandbox_workspace_write.network_access=true".to_string(),
    ];
    if o.system.len() <= MAX_ARG_INSTRUCTIONS {
        config.push(format!("developer_instructions={}", toml_str(o.system)));
    }
    if let Some((url, session)) = o.mcp {
        if o.own {
            config.push(format!("mcp_servers.tab.url={}", toml_str(url)));
            config.push("mcp_servers.tab.bearer_token_env_var=\"TAB_MCP_TOKEN\"".into());
            config.push(format!(
                "mcp_servers.tab.http_headers={{ \"X-Tab-Session\" = {} }}",
                toml_str(session)
            ));
        }
        for (name, url) in o.remote {
            config.push(format!("mcp_servers.{name}.url={}", toml_str(url)));
            config.push(format!(
                "mcp_servers.{name}.bearer_token_env_var=\"TAB_MCP_TOKEN\""
            ));
            config.push(format!(
                "mcp_servers.{name}.http_headers={{ \"X-Tab-Session\" = {} }}",
                toml_str(session)
            ));
        }
    }
    for c in config {
        a.extend(["-c".into(), c]);
    }
    if let Some(m) = o.model {
        a.extend(["--model".into(), m.into()]);
    }
    if let Some(id) = o.resume {
        a.extend(["resume".into(), id.into()]);
    }
    a.push("-".into());
    a
}

/// The prompt as Codex reads it: with the instructions in front when they
/// were too long for the command line.
fn stdin_prompt(system: &str, prompt: &str) -> String {
    if system.len() <= MAX_ARG_INSTRUCTIONS {
        prompt.to_string()
    } else {
        format!("<instructions>\n{system}\n</instructions>\n\n{prompt}")
    }
}

pub(super) async fn run_turn(
    hub: &Arc<AgentHub>,
    session: &Session,
    prompt: &str,
    system: &str,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    let cfg = hub.config_for(session);
    let exe = super::agent_exe(&cfg, AgentKind::Codex)?;
    let policy = hub.policy(session);
    let mcp = hub.mcp();
    let token = mcp
        .as_ref()
        .map(|m| crate::mcp::chat_token(&m.token, &session.id));
    let model = cfg.model_for(AgentKind::Codex);
    let strict = strict(cfg.strict, policy.trust);
    let remote = hub.remote_servers(policy.trust);
    let args = args(&Options {
        strict,
        enforced: crate::sandbox::current().enforced(),
        model,
        system,
        mcp: mcp.as_ref().map(|m| (m.url.as_str(), session.id.as_str())),
        remote: &remote,
        own: !hub.remote_mcp().in_cloud(),
        resume: session.agent_session_id.as_deref(),
    });
    let mut events = Parser::new(hub.clone(), session.id.clone(), model);
    events.strict = strict;
    events.allowed_mcp = remote.iter().map(|(name, _)| name.clone()).collect();
    let mut a = super::Args::default();
    a.args(&args);
    per_turn::run(
        hub,
        session,
        per_turn::Turn {
            name: "Codex",
            exe: &exe,
            args: a,
            env: token
                .map(|t| vec![("TAB_MCP_TOKEN".to_string(), t)])
                .unwrap_or_default(),
            stdin: Some(stdin_prompt(system, prompt)),
        },
        &mut events,
        cancel,
    )
    .await
}

/// Turns `codex exec --json` events into transcript entries.
pub(super) struct Parser {
    hub: Arc<AgentHub>,
    session_id: String,
    model: Option<String>,
    /// Items already shown when they started.
    shown: HashSet<String>,
    finished: bool,
    /// Strict mode: only tab's MCP server may be called.
    pub(super) strict: bool,
    /// Remote MCP servers tab added for this chat (strict allows them).
    pub(super) allowed_mcp: Vec<String>,
    leaked: Option<String>,
}

impl Parser {
    pub(super) fn new(hub: Arc<AgentHub>, session_id: String, model: Option<&str>) -> Self {
        Self {
            hub,
            session_id,
            model: model.map(str::to_string),
            shown: HashSet::new(),
            finished: false,
            strict: false,
            allowed_mcp: Vec::new(),
            leaked: None,
        }
    }

    /// The tool call an item stands for, as tab shows tools: a name and
    /// its input.
    fn tool(item: &Value) -> Option<(String, Value)> {
        let s = |k: &str| item[k].as_str().unwrap_or_default().to_string();
        Some(match item["type"].as_str()? {
            "command_execution" => ("Bash".into(), json!({ "command": s("command") })),
            "file_change" => {
                let paths: Vec<&str> = item["changes"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|c| c["path"].as_str())
                    .collect();
                (
                    "Edit".into(),
                    json!({ "file_path": paths.join(", "), "changes": item["changes"] }),
                )
            }
            "mcp_tool_call" => (
                format!("mcp__{}__{}", s("server"), s("tool")),
                item["arguments"].clone(),
            ),
            "web_search" => ("WebSearch".into(), json!({ "query": s("query") })),
            _ => return None,
        })
    }

    fn show_tool(&mut self, item: &Value) {
        if self.strict
            && item["type"] == "mcp_tool_call"
            && let Some(server) = item["server"]
                .as_str()
                .filter(|s| *s != "tab" && !self.allowed_mcp.iter().any(|a| a == s))
        {
            self.leaked = Some(format!("MCP server \"{server}\""));
        }
        let id = item["id"].as_str().unwrap_or_default().to_string();
        let Some((name, input)) = Self::tool(item) else {
            return;
        };
        if !self.shown.insert(id.clone()) {
            return;
        }
        self.hub.post(
            &self.session_id,
            "assistant",
            "tool",
            &super::summarize_tool(&name, &input),
            json!({ "tool": name, "id": id, "input": input }),
        );
    }

    fn completed(&mut self, item: &Value) {
        let sid = self.session_id.clone();
        let hub = self.hub.clone();
        let id = item["id"].as_str().unwrap_or_default();
        match item["type"].as_str().unwrap_or_default() {
            "agent_message" => {
                let text = item["text"].as_str().unwrap_or_default();
                if !text.trim().is_empty() {
                    hub.post(&sid, "assistant", "text", text, Value::Null);
                }
            }
            "error" => {
                let msg = item["message"]
                    .as_str()
                    .unwrap_or("Codex reported an error");
                hub.post(&sid, "system", "status", msg, Value::Null);
            }
            "command_execution" | "file_change" | "mcp_tool_call" | "web_search" => {
                self.show_tool(item);
                let (text, is_error) = match item["type"].as_str() {
                    Some("command_execution") => (
                        item["aggregated_output"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                        item["exit_code"].as_i64().is_some_and(|c| c != 0)
                            || item["status"] == "failed"
                            || item["status"] == "declined",
                    ),
                    Some("file_change") => (
                        item["changes"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .map(|c| {
                                format!(
                                    "{} {}",
                                    c["kind"].as_str().unwrap_or("update"),
                                    c["path"].as_str().unwrap_or_default()
                                )
                            })
                            .collect::<Vec<_>>()
                            .join("\n"),
                        item["status"] == "failed",
                    ),
                    Some("mcp_tool_call") => {
                        let text = match &item["error"]["message"] {
                            Value::String(e) => e.clone(),
                            _ => item["result"]["content"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .filter_map(|p| p["text"].as_str())
                                .collect::<Vec<_>>()
                                .join("\n"),
                        };
                        if let Some((name, input)) = Self::tool(item)
                            && crate::links::is_creator(&name, &input)
                        {
                            hub.attach_created_links(&sid, &text);
                        }
                        (
                            text,
                            item["status"] == "failed" || item["error"].is_object(),
                        )
                    }
                    _ => (String::new(), false),
                };
                hub.post(
                    &sid,
                    "tool",
                    "tool_result",
                    &text.chars().take(4000).collect::<String>(),
                    json!({ "tool_use_id": id, "is_error": is_error }),
                );
            }
            _ => {}
        }
    }

    fn handle_event(&mut self, v: &Value) {
        let sid = self.session_id.clone();
        let hub = self.hub.clone();
        match v["type"].as_str().unwrap_or_default() {
            "thread.started" => {
                if let Some(id) = v["thread_id"].as_str() {
                    hub.update(&sid, None, Some(id));
                }
            }
            "item.started" => self.show_tool(&v["item"]),
            "item.completed" => self.completed(&v["item"]),
            "turn.completed" => {
                self.finished = true;
                let u = &v["usage"];
                let tokens = |k: &str| u[k].as_i64().unwrap_or(0);
                let usage = crate::store::NewUsage {
                    purpose: "agent",
                    session_id: Some(sid.clone()),
                    agent: AgentKind::Codex.as_str().into(),
                    model: self.model.clone(),
                    billing: "unknown",
                    cost_usd: 0.0,
                    input_tokens: tokens("input_tokens"),
                    output_tokens: tokens("output_tokens"),
                };
                if let Err(e) = hub.store.usage_add(&usage, chrono::Utc::now()) {
                    tracing::warn!(error = %e, "couldn't store usage");
                }
                hub.post(&sid, "system", "result", "", json!({ "usage": u }));
                hub.update(&sid, Some(SessionStatus::Idle), None);
            }
            "turn.failed" => {
                self.finished = true;
                let msg = v["error"]["message"].as_str().unwrap_or("The turn failed");
                hub.post(&sid, "system", "error", msg, Value::Null);
                hub.post(&sid, "system", "result", "", Value::Null);
                hub.update(&sid, Some(SessionStatus::Failed), None);
            }
            // Reconnects and other notices; a fatal one ends in `turn.failed`.
            "error" => {
                if let Some(msg) = v["message"].as_str() {
                    hub.post(&sid, "system", "status", msg, Value::Null);
                }
            }
            _ => {}
        }
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

/// One answer from a model through Codex, for background helpers: no saved
/// session, none of the person's config, nothing it may write.
pub(super) async fn complete(
    hub: &Arc<AgentHub>,
    system: &str,
    prompt: &str,
    model: Option<&str>,
) -> anyhow::Result<String> {
    let exe = super::agent_exe(&hub.config(), AgentKind::Codex)?;
    let mut a: Vec<std::ffi::OsString> = [
        "exec",
        "--json",
        "--ephemeral",
        "--skip-git-repo-check",
        "--ignore-user-config",
        "--ignore-rules",
        "--sandbox",
        "read-only",
        "-c",
        "approval_policy=\"never\"",
    ]
    .map(Into::into)
    .into();
    if system.len() <= MAX_ARG_INSTRUCTIONS {
        a.push("-c".into());
        a.push(format!("developer_instructions={}", toml_str(system)).into());
    }
    if let Some(m) = model {
        a.push("--model".into());
        a.push(m.into());
    }
    a.push("-".into());
    let out = per_turn::capture(
        &exe,
        &a,
        Some(&stdin_prompt(system, prompt)),
        std::time::Duration::from_secs(90),
    )
    .await?;
    Ok(last_message(&out))
}

/// The last thing the agent said in a run's events.
fn last_message(jsonl: &str) -> String {
    jsonl
        .lines()
        .rev()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| v["type"] == "item.completed" && v["item"]["type"] == "agent_message")
        .find_map(|v| v["item"]["text"].as_str().map(str::to_string))
        .unwrap_or_default()
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AgentConfig;
    use crate::store::Store;
    use chrono::Utc;

    fn opts() -> Options<'static> {
        Options {
            strict: false,
            enforced: false,
            model: None,
            system: "Be brief.",
            mcp: None,
            remote: &[],
            own: true,
            resume: None,
        }
    }

    #[test]
    fn a_new_turn_reads_its_prompt_from_stdin() {
        let a = args(&opts());
        assert_eq!(&a[..3], ["exec", "--json", "--skip-git-repo-check"]);
        assert_eq!(a.last().unwrap(), "-");
        assert!(a.contains(&"developer_instructions=\"Be brief.\"".to_string()));
        assert!(a.windows(2).any(|w| w == ["--sandbox", "workspace-write"]));
        assert!(!a.contains(&"--ignore-user-config".to_string()));
    }

    #[test]
    fn options_come_before_resume() {
        let a = args(&Options {
            resume: Some("thread-1"),
            model: Some("gpt-6-sol"),
            strict: true,
            enforced: true,
            mcp: Some(("http://127.0.0.1:9/mcp", "s1")),
            remote: &[("linear".into(), "http://127.0.0.1:9/mcp/linear".into())],
            ..opts()
        });
        let at = a.iter().position(|x| x == "resume").unwrap();
        assert_eq!(&a[at..], ["resume", "thread-1", "-"]);
        for flag in ["--model", "--ignore-user-config", "-c"] {
            assert!(a.iter().position(|x| x == flag).unwrap() < at, "{flag}");
        }
        // tab's own sandbox bounds it; Codex's can't nest inside.
        assert!(
            a.windows(2)
                .any(|w| w == ["--sandbox", "danger-full-access"])
        );
        assert!(a.contains(&"mcp_servers.tab.url=\"http://127.0.0.1:9/mcp\"".to_string()));
        assert!(
            a.contains(&"mcp_servers.linear.url=\"http://127.0.0.1:9/mcp/linear\"".to_string())
        );
        assert!(
            a.contains(&"mcp_servers.linear.bearer_token_env_var=\"TAB_MCP_TOKEN\"".to_string())
        );
        assert!(
            a.contains(&"mcp_servers.tab.http_headers={ \"X-Tab-Session\" = \"s1\" }".to_string())
        );
        // The token stays off the command line.
        assert!(a.iter().all(|x| !x.contains("Bearer")));
    }

    #[test]
    fn instructions_are_toml_strings() {
        let a = args(&Options {
            system: "Say \"hi\"\nthen\\stop",
            ..opts()
        });
        let c = a
            .iter()
            .find(|x| x.starts_with("developer_instructions="))
            .unwrap();
        assert_eq!(c, r#"developer_instructions="Say \"hi\"\nthen\\stop""#);
    }

    #[test]
    fn long_instructions_ride_in_the_prompt() {
        let long = "x".repeat(MAX_ARG_INSTRUCTIONS + 1);
        let a = args(&Options {
            system: &long,
            ..opts()
        });
        assert!(a.iter().all(|x| !x.starts_with("developer_instructions")));
        assert!(stdin_prompt(&long, "hi").ends_with("</instructions>\n\nhi"));
        assert_eq!(stdin_prompt("short", "hi"), "hi");
    }

    #[test]
    fn the_last_message_is_the_reply() {
        let out = r#"{"type":"thread.started","thread_id":"t"}
{"type":"item.completed","item":{"id":"item_0","type":"reasoning","text":"thinking"}}
{"type":"item.completed","item":{"id":"item_1","type":"agent_message","text":"first"}}
{"type":"item.completed","item":{"id":"item_2","type":"agent_message","text":" {\"title\":\"Fix\"} "}}
{"type":"turn.completed","usage":{"input_tokens":1,"cached_input_tokens":0,"output_tokens":1}}"#;
        assert_eq!(last_message(out), "{\"title\":\"Fix\"}");
    }

    #[tokio::test]
    async fn parses_an_exec_turn() {
        let store = Arc::new(Store::in_memory().unwrap());
        let (tx, _rx) = tokio::sync::broadcast::channel(64);
        let hub = AgentHub::new(
            store.clone(),
            tx,
            AgentConfig::default(),
            std::env::temp_dir().join("tab-codex-test"),
        );
        let s = store
            .session_create(None, AgentKind::Codex, "t", ".", Utc::now())
            .unwrap();
        let mut p = Parser::new(hub.clone(), s.id.clone(), Some("gpt-6-sol"));
        let lines = [
            r#"{"type":"thread.started","thread_id":"0199-thread"}"#,
            r#"{"type":"turn.started"}"#,
            r#"{"type":"item.started","item":{"id":"item_1","type":"command_execution","command":"bash -lc ls","aggregated_output":"","exit_code":null,"status":"in_progress"}}"#,
            r#"{"type":"item.completed","item":{"id":"item_1","type":"command_execution","command":"bash -lc ls","aggregated_output":"Cargo.toml\n","exit_code":0,"status":"completed"}}"#,
            r#"{"type":"item.completed","item":{"id":"item_2","type":"file_change","changes":[{"path":"src/lib.rs","kind":"update"}],"status":"completed"}}"#,
            r#"{"type":"item.completed","item":{"id":"item_3","type":"agent_message","text":"Done."}}"#,
            r#"{"type":"turn.completed","usage":{"input_tokens":1200,"cached_input_tokens":800,"output_tokens":40}}"#,
        ];
        for l in lines {
            p.handle(&serde_json::from_str(l).unwrap());
        }
        assert!(p.finished());
        let got = store.session_get(&s.id).unwrap().unwrap();
        assert_eq!(got.agent_session_id.as_deref(), Some("0199-thread"));
        assert_eq!(got.status, SessionStatus::Idle);
        let m = store.messages(&s.id).unwrap();
        let kinds: Vec<_> = m.iter().map(|m| m.kind.as_str()).collect();
        assert_eq!(
            kinds,
            [
                "tool",
                "tool_result",
                "tool",
                "tool_result",
                "text",
                "result"
            ]
        );
        assert_eq!(m[0].content, "Bash: bash -lc ls");
        assert_eq!(m[1].content, "Cargo.toml\n");
        assert_eq!(m[2].content, "Edit: src/lib.rs");
        assert_eq!(m[4].content, "Done.");
    }

    #[tokio::test]
    async fn strict_mode_catches_another_mcp_server() {
        let store = Arc::new(Store::in_memory().unwrap());
        let (tx, _rx) = tokio::sync::broadcast::channel(64);
        let hub = AgentHub::new(
            store.clone(),
            tx,
            AgentConfig::default(),
            std::env::temp_dir().join("tab-codex-test-3"),
        );
        let s = store
            .session_create(None, AgentKind::Codex, "t", ".", Utc::now())
            .unwrap();
        let mut p = Parser::new(hub.clone(), s.id.clone(), None);
        p.strict = true;
        let call = |server: &str| json!({ "type": "item.started", "item": { "id": server, "type": "mcp_tool_call", "server": server, "tool": "search", "arguments": {}, "status": "in_progress" } });
        p.handle(&call("tab"));
        assert_eq!(p.leaked(), None);
        p.handle(&call("linear"));
        assert_eq!(p.leaked().as_deref(), Some("MCP server \"linear\""));
    }

    #[tokio::test]
    async fn a_failed_turn_says_why() {
        let store = Arc::new(Store::in_memory().unwrap());
        let (tx, _rx) = tokio::sync::broadcast::channel(64);
        let hub = AgentHub::new(
            store.clone(),
            tx,
            AgentConfig::default(),
            std::env::temp_dir().join("tab-codex-test-2"),
        );
        let s = store
            .session_create(None, AgentKind::Codex, "t", ".", Utc::now())
            .unwrap();
        let mut p = Parser::new(hub.clone(), s.id.clone(), None);
        p.handle(&json!({ "type": "turn.failed", "error": { "message": "You've hit your usage limit." } }));
        assert!(p.finished());
        assert_eq!(
            store.session_get(&s.id).unwrap().unwrap().status,
            SessionStatus::Failed
        );
        assert!(
            store
                .messages(&s.id)
                .unwrap()
                .iter()
                .any(|m| m.kind == "error" && m.content.contains("usage limit"))
        );
    }
}
