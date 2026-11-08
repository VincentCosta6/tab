//! One-shot calls to a small model through the agent the person already set
//! up, for background helpers like suggested replies and the titler. Claude
//! Code runs with no tools, no MCP, no settings and no saved session, so the
//! call is cheap and can't touch anything.

use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use super::AgentHub;
use crate::config::AgentConfig;
use crate::model::AgentKind;

pub(super) const DEFAULT_MODEL: &str = "claude-haiku-5-5";

pub(super) struct Ask<'a> {
    /// The harness to ask; `None` is the one chats use.
    pub agent: Option<AgentKind>,
    /// `None` is the harness's small model (`harnesses.json`), else the
    /// chat model.
    pub model: Option<&'a str>,
    pub system: &'a str,
    pub prompt: &'a str,
    /// Structured output. Without one the reply is plain text.
    pub schema: Option<Value>,
    /// Usage label, e.g. `suggestion`.
    pub purpose: &'static str,
    pub session_id: Option<&'a str>,
    pub timeout: Duration,
}

/// The reply: the structured object when a schema was given, else a string.
/// `Value::Null` when the model returned an error result.
pub(super) async fn complete(
    hub: &Arc<AgentHub>,
    cfg: &AgentConfig,
    ask: Ask<'_>,
) -> anyhow::Result<Value> {
    match ask.agent.or(cfg.kind) {
        Some(AgentKind::ClaudeCode) => via_claude(hub, cfg, ask).await,
        Some(kind) => {
            let mut system = ask.system.to_string();
            if let Some(schema) = &ask.schema {
                system.push_str(&format!(
                    "\n\nReply with only a JSON object matching this schema, no prose:\n{schema}"
                ));
            }
            let model = ask
                .model
                .or(super::catalog::harness(kind).small_model.as_deref())
                .or(cfg.model_for(kind));
            let text = match kind {
                AgentKind::OpenCode => {
                    super::opencode::complete(hub, &system, ask.prompt, model).await?
                }
                AgentKind::Codex => super::codex::complete(hub, &system, ask.prompt, model).await?,
                AgentKind::Pi => super::pi::complete(hub, &system, ask.prompt, model).await?,
                AgentKind::ClaudeCode => unreachable!("handled above"),
            };
            Ok(match ask.schema {
                Some(_) => parse_json_reply(&text).unwrap_or(Value::Null),
                None => Value::String(text),
            })
        }
        None => anyhow::bail!("no agent set up"),
    }
}

async fn via_claude(hub: &Arc<AgentHub>, cfg: &AgentConfig, ask: Ask<'_>) -> anyhow::Result<Value> {
    let exe = super::agent_exe(cfg, AgentKind::ClaudeCode)?;
    let schema = ask.schema.as_ref().map(Value::to_string);
    let mut cmd = super::command(&exe);
    cmd.args([
        "-p",
        "--output-format",
        "json",
        "--model",
        ask.model.unwrap_or(DEFAULT_MODEL),
        "--tools",
        "",
        "--strict-mcp-config",
        "--no-session-persistence",
        "--setting-sources",
        "",
        "--system-prompt",
        ask.system,
    ]);
    if let Some(s) = &schema {
        cmd.args(["--json-schema", s]);
    }
    cmd.current_dir(std::env::temp_dir())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = cmd.spawn()?;
    {
        use tokio::io::AsyncWriteExt;
        let mut stdin = child.stdin.take().expect("piped");
        stdin.write_all(ask.prompt.as_bytes()).await?;
    }
    let out = tokio::time::timeout(ask.timeout, child.wait_with_output()).await??;
    let v: Value = serde_json::from_slice(&out.stdout)?;
    crate::usage::record_claude_result(&hub.store, &v, ask.purpose, ask.session_id, "unknown");
    if v["is_error"].as_bool().unwrap_or(false) {
        return Ok(Value::Null);
    }
    let text = v["result"].as_str().unwrap_or_default();
    Ok(match schema {
        None => Value::String(text.to_string()),
        Some(_) if !v["structured_output"].is_null() => v["structured_output"].clone(),
        Some(_) => parse_json_reply(text).unwrap_or(Value::Null),
    })
}

/// The first JSON object in a reply, tolerating code fences or stray prose.
fn parse_json_reply(text: &str) -> Option<Value> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    serde_json::from_str(text.get(start..=end)?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_replies_survive_fences_and_prose() {
        let v = parse_json_reply("Sure:\n```json\n{\"title\": \"Fix login\"}\n```").unwrap();
        assert_eq!(v["title"], "Fix login");
        assert!(parse_json_reply("no json here").is_none());
    }
}
