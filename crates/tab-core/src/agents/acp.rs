//! Agent Client Protocol (ACP) adapter: one client for every harness that
//! speaks ACP, natively (OpenCode's `opencode acp`) or through an adapter
//! program (claude-agent-acp, codex-acp, pi-acp). What to run is data, the
//! `acp` entry of the harness in `harnesses.json`.
//!
//! One process per turn, like `per_turn`: it starts under the chat's
//! sandbox, answers `initialize`, then loads the chat's ACP session (or
//! starts one) and sends the message as `session/prompt`. `session/update`
//! notifications become the chat's deltas and messages, and
//! `session/request_permission` becomes an approval card.
//!
//! What ACP leaves to tab stays here: the sandbox (tab launches the
//! process), strict mode (the entry's strict arguments and environment, and
//! a check of the tools that show up), and which adapters tab offers.
//!
//! Messages are typed by `agent-client-protocol-schema` (Apache-2.0). The
//! SDK's own connection isn't used: it runs on a single-threaded executor,
//! and tab has to spawn the process itself.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use agent_client_protocol_schema::ProtocolVersion;
use agent_client_protocol_schema::v1 as acp;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use super::catalog;
use super::{AgentHub, Decision, SlashCommand, SlashKind, Steer};
use crate::model::{AgentKind, Session, SessionStatus};
use crate::sandbox::Trust;

/// How long `initialize` and the session may take to answer.
const START_TIMEOUT: Duration = Duration::from_secs(60);
/// After `session/cancel`, how long the agent gets to end the turn itself.
const CANCEL_GRACE: Duration = Duration::from_secs(3);

/// Whether `kind`'s chats run through ACP: `harnesses.json` says how, and
/// either it's the harness's only way (OpenCode) or, for an entry still in
/// preview, the person turned it on. A strict chat stays on the harness's
/// own adapter when the ACP one can't keep the person's setup out.
pub(crate) fn enabled(cfg: &crate::config::AgentConfig, kind: AgentKind, strict: bool) -> bool {
    match &catalog::harness(kind).acp {
        Some(spec) if strict && !spec.strict => false,
        Some(spec) if spec.preview => cfg.acp.iter().any(|k| k == kind.as_str()),
        Some(_) => true,
        None => false,
    }
}

pub(super) async fn run_turn(
    hub: &Arc<AgentHub>,
    session: &Session,
    prompt: &str,
    system: &str,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    // Messages sent mid-turn: agents that take them (`acp.steering`) get
    // them now; the rest, and anything left over, run as the next turn.
    hub.steering_close(&session.id);
    let mut queued = hub.steering_open(&session.id);
    let result = run(hub, session, prompt, system, &mut queued, cancel).await;
    hub.watch_unanswered(&session.id, None);
    let mut later = Vec::new();
    while let Ok(s) = queued.try_recv() {
        if let Steer::Message(m) = s {
            later.push(m);
        }
    }
    if !later.is_empty() {
        hub.followups
            .lock()
            .unwrap()
            .entry(session.id.clone())
            .or_default()
            .extend(later);
    }
    result
}

async fn run(
    hub: &Arc<AgentHub>,
    session: &Session,
    prompt: &str,
    system: &str,
    steering: &mut mpsc::UnboundedReceiver<Steer>,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    let kind = session.agent;
    let harness = catalog::harness(kind);
    let spec = harness
        .acp
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("{} has no ACP entry", harness.name))?;
    let cfg = hub.config_for(session);
    let mut policy = hub.policy(session);
    let strict = cfg.strict || policy.trust != Trust::Person;
    // Claude Code keeps tab's own setup (see `claude::acp_setup`), and
    // narrows the sandbox to its conversation before anything starts.
    let mut claude = match kind {
        AgentKind::ClaudeCode => Some(super::claude::acp_setup(
            hub,
            session,
            &cfg,
            &mut policy,
            system,
        )?),
        _ => None,
    };
    let start = match &spec.command {
        // Installed now if it's missing (the first chat after an update).
        Some(c) if spec.adapter.is_some() => {
            super::adapter::ensure(kind, |l| tracing::info!("{l}"))
                .await
                .map_err(|e| {
                    anyhow::anyhow!(
                        "tab couldn't set up {c}, which {} runs through: {e:#}",
                        harness.name
                    )
                })?
        }
        Some(c) => super::adapter::Launch::program(super::resolve(c)?),
        None => super::adapter::Launch::program(super::agent_exe(&cfg, kind)?),
    };
    let exe = start.program;
    let mut args = start.args;
    args.extend(spec.args_for(strict));
    policy.readable.extend(start.readable);
    let cwd = std::path::Path::new(&session.cwd);
    let mut prepared = crate::sandbox::current()
        .command(&exe, &args, cwd, &policy)
        .await?;
    hub.add_shims(session, &mut prepared.cmd);
    for (k, v) in spec.env_for(strict) {
        prepared.cmd.env(k, v);
    }
    if kind == AgentKind::Codex {
        // The person's own Codex (its version, its sign-in), not the copy
        // codex-acp bundles. Where tab's sandbox bounds the process, Codex's
        // own can't start inside it, as in codex.rs; elsewhere Codex keeps
        // to the workspace and asks, which comes to tab as an approval card.
        prepared
            .cmd
            .env("CODEX_PATH", super::agent_exe(&cfg, kind)?);
        let mode = if prepared.enforced {
            "agent-full-access"
        } else {
            "workspace-write"
        };
        prepared.cmd.env("INITIAL_AGENT_MODE", mode);
    }
    // Servers the person set up that strict mode turned off; a tool from
    // one of them stops the turn.
    let (mut env, foreign) = launch(kind, strict, cwd, policy.trust, Use::Chat)?;
    if let Some(c) = &claude {
        env.extend(c.env.iter().cloned());
    }
    for (k, v) in env {
        prepared.cmd.env(k, v);
    }
    prepared
        .cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    tracing::debug!(session = %session.id, harness = harness.name, resume = ?session.agent_session_id, sandboxed = prepared.enforced, "starting ACP turn");
    let sandboxed = prepared.enforced;
    let mut child = prepared.spawn().map_err(|e| {
        anyhow::anyhow!("starting {} ({}) failed: {e}", harness.name, exe.display())
    })?;
    let stdin = child.stdin.take().expect("piped");
    let stdout = child.stdout.take().expect("piped");
    let mut stderr = child.stderr.take().expect("piped");
    let err_task = tokio::spawn(async move {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s).await;
        s
    });
    let (conn, mut incoming) = Conn::start(stdin, stdout);

    let mut events = Translator::new(hub.clone(), session.id.clone(), kind);
    events.model = cfg.model_for(kind).map(str::to_string);
    if let Ok(dir) = hub.session_dir(&session.id) {
        let file = dir.join("acp-cost");
        events.cost_before = std::fs::read_to_string(&file)
            .ok()
            .and_then(|c| c.trim().parse().ok())
            .unwrap_or(0.0);
        events.cost_file = Some(file);
    }
    events.strict = strict;
    events.mcp_tool_name = spec.mcp_tool_name.clone();
    events.foreign = foreign;
    events.allowed = vec!["tab".to_string()];
    let session_meta = claude.as_ref().map(|c| c.meta.clone());
    let claude_resume = claude.as_ref().map(|c| c.resume.clone());
    let claude_mode = claude.as_ref().map(|c| c.mode.clone());
    // Claude Code's chat comes from its own messages (`_claude/sdkMessage`).
    events.raw = claude.take().map(|c| c.parser);
    hub.watch_unanswered(&session.id, events.raw.as_ref().map(|p| p.unanswered()));

    let outcome: anyhow::Result<Option<Result<Value, RpcError>>> = async {
        // Start: initialize, then the chat's session. Updates that replay an
        // old session's history while it loads aren't new.
        events.replaying = true;
        let init = drive(
            &conn,
            &mut incoming,
            &mut events,
            &cancel,
            conn.request_value("initialize", initialize_params()),
        )
        .await?;
        let Some(init) = init else { return Ok(None) };
        let init = init?;
        let steers = init["_meta"]["steering"]["supported"] == true;
        let init: acp::InitializeResponse = serde_json::from_value(init)?;
        let caps = &init.agent_capabilities;

        let mut servers = Vec::new();
        if let Some(mcp) = hub.mcp().filter(|_| caps.mcp_capabilities.http) {
            let headers = vec![
                acp::HttpHeader::new(
                    "Authorization",
                    format!("Bearer {}", crate::mcp::chat_token(&mcp.token, &session.id)),
                ),
                acp::HttpHeader::new("X-Tab-Session", session.id.clone()),
            ];
            if !hub.remote_mcp().in_cloud() {
                servers.push(acp::McpServer::Http(
                    acp::McpServerHttp::new("tab", mcp.url.clone()).headers(headers.clone()),
                ));
            }
            for (name, url) in hub.remote_servers(policy.trust) {
                events.allowed.push(name.clone());
                servers.push(acp::McpServer::Http(
                    acp::McpServerHttp::new(name, url).headers(headers.clone()),
                ));
            }
        }
        let system_prompt = caps
            .session_capabilities
            .system_prompt
            .is_some()
            .then(|| acp::SystemPrompt::new(vec![text_block(system)]));

        // A rewound Claude Code chat may start over (see `claude::Rewind`).
        let resume = claude_resume
            .unwrap_or_else(|| session.agent_session_id.clone())
            .filter(|_| caps.load_session);
        let with_meta = |mut v: Value| {
            if let (Some(m), Some(o)) = (&session_meta, v.as_object_mut()) {
                o.insert("_meta".into(), m.clone());
            }
            v
        };
        let acp_session = match resume {
            Some(id) => {
                let load = acp::LoadSessionRequest::new(id.clone(), session.cwd.clone())
                    .mcp_servers(servers)
                    .system_prompt(system_prompt.clone());
                let Some(r) = drive(
                    &conn,
                    &mut incoming,
                    &mut events,
                    &cancel,
                    conn.request_value(
                        "session/load",
                        with_meta(serde_json::to_value(load).unwrap_or_default()),
                    ),
                )
                .await?
                else {
                    return Ok(None);
                };
                let mut r = r?;
                if events.raw.is_some() {
                    save_subagents(hub, &r);
                }
                // The chat's model can change between turns.
                if let Some(model) = cfg.model_for(kind) {
                    if let Some(o) = r.as_object_mut() {
                        o.insert("sessionId".into(), json!(id));
                    }
                    set_model(&conn, &r, model).await;
                }
                id
            }
            None => {
                let new = acp::NewSessionRequest::new(session.cwd.clone())
                    .mcp_servers(servers)
                    .system_prompt(system_prompt.clone());
                let Some(r) = drive(
                    &conn,
                    &mut incoming,
                    &mut events,
                    &cancel,
                    conn.request_value(
                        "session/new",
                        with_meta(serde_json::to_value(new).unwrap_or_default()),
                    ),
                )
                .await?
                else {
                    return Ok(None);
                };
                let r = r?;
                if events.raw.is_some() {
                    save_subagents(hub, &r);
                }
                let id = serde_json::from_value::<acp::NewSessionResponse>(r.clone())?
                    .session_id
                    .0
                    .to_string();
                if let Some(model) = cfg.model_for(kind) {
                    set_model(&conn, &r, model).await;
                }
                id
            }
        };
        if let Some(mode) = &claude_mode {
            set_mode(&conn, &acp_session, mode).await;
        }
        // Claude Code's conversation is the one tab named, which its own
        // messages report (the adapter's id for a new one differs).
        if events.raw.is_none() {
            hub.update(&session.id, None, Some(&acp_session));
        }
        events.replaying = false;

        // An agent that can't take a system prompt gets tab's instructions
        // with the chat's first message.
        let first = session.agent_session_id.is_none();
        let text = if system_prompt.is_none() && first && !system.trim().is_empty() {
            format!("<tab-instructions>\n{system}\n</tab-instructions>\n\n{prompt}")
        } else {
            prompt.to_string()
        };
        let request = acp::PromptRequest::new(acp_session.clone(), vec![text_block(&text)]);
        let r = prompt_turn(
            &conn,
            &mut incoming,
            &mut events,
            &cancel,
            conn.request("session/prompt", request),
            match spec.steering {
                // An agent that says it takes `_session/steering` gets it.
                catalog::Steering::Extension if steers => Some((&mut *steering, true)),
                catalog::Steering::Prompt => Some((&mut *steering, false)),
                _ => None,
            },
            &acp_session,
        )
        .await?;
        // Background work Claude Code started (a command, a subagent, a
        // watcher) runs on after the reply, and may start a turn of its own
        // when it ends: the chat shows Monitoring until it's all done.
        if r.as_ref().is_some_and(|r| r.is_ok())
            && events
                .raw
                .as_ref()
                .is_some_and(|p| !p.background.is_empty())
        {
            hub.update(&session.id, Some(SessionStatus::Monitoring), None);
            loop {
                let busy = events
                    .raw
                    .as_ref()
                    .is_some_and(|p| !p.background.is_empty() || !p.finished());
                if !busy {
                    break;
                }
                tokio::select! {
                    m = incoming.recv() => if !handle(&conn, &mut events, m)? { break },
                    _ = cancel.cancelled() => break,
                }
            }
            if !cancel.is_cancelled() {
                hub.update(&session.id, Some(SessionStatus::Idle), None);
            }
        }
        match r {
            Some(r) => Ok(Some(r)),
            None => {
                // Stopped: ask the agent to end the turn, then make sure.
                conn.notify("session/cancel", acp::CancelNotification::new(acp_session));
                let _ = tokio::time::timeout(CANCEL_GRACE, async {
                    while let Some(m) = incoming.recv().await {
                        if let Incoming::Request { id, .. } = m {
                            conn.respond(id, Ok(cancelled_permission()));
                        }
                    }
                })
                .await;
                Ok(None)
            }
        }
    }
    .await;

    // The reply is in (or the turn is over): read what the agent logged
    // before deciding how it ended.
    child.kill_tree();
    let _ = child.wait().await;
    let stderr = err_task.await.unwrap_or_default();
    let finish = match outcome {
        Ok(Some(Ok(r))) => {
            let usage = r.get("usage").cloned();
            let r: acp::PromptResponse = serde_json::from_value(r)?;
            events.usage = usage;
            let stop = match empty_turn(&events, &r.stop_reason) {
                // OpenCode ends a turn whose model call failed as `end_turn`
                // with nothing in it, and logs the error.
                true => Err(format!(
                    "{} ended the turn without replying.{}",
                    harness.name,
                    super::per_turn::tail(&stderr)
                )),
                false => Ok(r.stop_reason),
            };
            events.finish(stop);
            Ok(())
        }
        Ok(Some(Err(e))) => {
            events.finish(Err(e.to_string()));
            Ok(())
        }
        Ok(None) => Ok(()),
        Err(e) => Err(e),
    };
    if let Some(what) = events.leaked.take() {
        anyhow::bail!(
            "Strict mode stopped this turn: {} used {what}, which isn't one of tab's connections.",
            harness.name
        );
    }
    finish.map_err(|e| {
        anyhow::anyhow!(
            "{} stopped before finishing: {e}{}{}",
            harness.name,
            super::per_turn::tail(&stderr),
            match kind {
                AgentKind::ClaudeCode => super::claude::startup_hint(&stderr, sandboxed),
                _ => "",
            }
        )
    })
}

/// A harness's environment, and the person's MCP servers strict mode
/// turned off.
type Launch = (Vec<(String, String)>, Vec<String>);

/// What a turn is for, where it changes how the agent starts.
#[derive(Clone, Copy, PartialEq)]
enum Use {
    Chat,
    /// A background helper's answer: it may read, never act.
    OneShot,
}

/// What each harness needs in its environment that its `harnesses.json`
/// entry can't say, and the person's own MCP servers that strict mode
/// turned off (a tool from one stops the turn).
fn launch(
    kind: AgentKind,
    strict: bool,
    cwd: &std::path::Path,
    trust: Trust,
    purpose: Use,
) -> anyhow::Result<Launch> {
    let mut env = Vec::new();
    let mut foreign = Vec::new();
    match kind {
        AgentKind::OpenCode if strict => {
            foreign = super::opencode::own_mcp_servers(cwd, trust);
            let off: serde_json::Map<_, _> = foreign
                .iter()
                .map(|n| (n.clone(), json!({ "enabled": false })))
                .collect();
            env.push((
                "OPENCODE_CONFIG_CONTENT".into(),
                json!({ "mcp": off }).to_string(),
            ));
        }
        AgentKind::Codex => {
            // Codex's sandbox can't run inside tab's, so where tab's is
            // enforced it bounds the agent and Codex never asks (as before);
            // elsewhere Codex keeps edits to the folder and asks the person
            // for the rest.
            let mode = match purpose {
                Use::OneShot => "read-only",
                Use::Chat if crate::sandbox::current().enforced() => "agent-full-access",
                Use::Chat => "workspace-write",
            };
            env.push(("INITIAL_AGENT_MODE".into(), mode.into()));
            if strict {
                // Codex's own config folder without the person's config,
                // instructions, rules, plugins, skills or memories.
                let home = super::codex::strict_home()?;
                env.push(("CODEX_HOME".into(), home.to_string_lossy().into_owned()));
            }
        }
        _ => {}
    }
    Ok((env, foreign))
}

/// One answer for background helpers (titles, suggested replies): a
/// throwaway session in an empty folder, with none of tab's MCP servers,
/// and every permission refused, so the agent answers instead of acting.
pub(super) async fn complete(
    hub: &Arc<AgentHub>,
    kind: AgentKind,
    system: &str,
    prompt: &str,
    model: Option<&str>,
) -> anyhow::Result<String> {
    let harness = catalog::harness(kind);
    let spec = harness
        .acp
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("{} has no ACP entry", harness.name))?;
    let cfg = hub.config();
    let exe = match &spec.command {
        Some(c) => super::resolve(c)?,
        None => super::agent_exe(&cfg, kind)?,
    };
    let dir = std::env::temp_dir().join(format!("tab-ask-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir)?;
    let result = async {
        let sandbox = crate::sandbox::current();
        let policy = sandbox.chat_policy();
        let mut prepared = sandbox
            .command(&exe, &spec.args_for(true), &dir, &policy)
            .await?;
        for (k, v) in spec.env_for(true) {
            prepared.cmd.env(k, v);
        }
        for (k, v) in launch(kind, true, &dir, Trust::Person, Use::OneShot)?.0 {
            prepared.cmd.env(k, v);
        }
        prepared
            .cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = prepared.spawn()?;
        let (conn, mut incoming) =
            Conn::start(child.stdin.take().expect("piped"), child.stdout.take().expect("piped"));
        let answer = tokio::time::timeout(Duration::from_secs(90), async {
            let init: acp::InitializeResponse = serde_json::from_value(
                conn.request(
                    "initialize",
                    acp::InitializeRequest::new(ProtocolVersion::V1)
                        .client_capabilities(acp::ClientCapabilities::default())
                        .client_info(acp::Implementation::new("tab", env!("CARGO_PKG_VERSION"))),
                )
                .await?,
            )?;
            let takes_system = init.agent_capabilities.session_capabilities.system_prompt.is_some();
            let mut new = acp::NewSessionRequest::new(dir.clone());
            if takes_system {
                new = new.system_prompt(acp::SystemPrompt::new(vec![text_block(system)]));
            }
            let r = conn.request("session/new", new).await?;
            let id = serde_json::from_value::<acp::NewSessionResponse>(r.clone())?
                .session_id
                .0
                .to_string();
            if let Some(m) = model {
                set_model(&conn, &r, m).await;
            }
            let text = if takes_system {
                prompt.to_string()
            } else {
                format!("<instructions>\n{system}\n</instructions>\n\n{prompt}")
            };
            let reply = conn.request(
                "session/prompt",
                acp::PromptRequest::new(id, vec![text_block(&text)]),
            );
            tokio::pin!(reply);
            let mut out = String::new();
            loop {
                tokio::select! {
                    r = &mut reply => { r?; break; }
                    m = incoming.recv() => match m {
                        Some(Incoming::Notification { method, params }) if method == "session/update" => {
                            if let Ok(n) = serde_json::from_value::<acp::SessionNotification>(params)
                                && let acp::SessionUpdate::AgentMessageChunk(c) = n.update
                                && let acp::ContentBlock::Text(t) = c.content
                            {
                                out.push_str(&t.text);
                            }
                        }
                        Some(Incoming::Request { id, method, .. }) => {
                            if method == "session/request_permission" {
                                conn.respond(id, Ok(cancelled_permission()));
                            } else {
                                conn.respond(id, Err(RpcError { code: -32601, message: format!("tab doesn't offer {method}") }));
                            }
                        }
                        Some(_) => {}
                        None => anyhow::bail!("it closed the connection"),
                    },
                }
            }
            anyhow::Ok(out)
        })
        .await;
        child.kill_tree();
        let _ = child.wait().await;
        match answer {
            Ok(r) => r,
            Err(_) => anyhow::bail!("{} took too long to answer", harness.name),
        }
    }
    .await;
    let _ = std::fs::remove_dir_all(&dir);
    let answer = result?.trim().to_string();
    anyhow::ensure!(!answer.is_empty(), "{} answered with nothing", harness.name);
    Ok(answer)
}

/// A turn that "ended" with no text, no tool call and no tokens out: the
/// model call failed somewhere the protocol didn't say.
fn empty_turn(events: &Translator, stop: &acp::StopReason) -> bool {
    let out = events
        .usage
        .as_ref()
        .and_then(|u| u["outputTokens"].as_i64())
        .unwrap_or(0);
    events.raw.is_none() && *stop == acp::StopReason::EndTurn && !events.said_anything && out == 0
}

/// Claude Code's own agents (the person's and the project's), for the `@`
/// menu: claude-agent-acp offers them as the session's `agent` option.
fn save_subagents(hub: &AgentHub, r: &Value) {
    let Some(agent) = r["configOptions"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|o| o["id"] == "agent")
    else {
        return;
    };
    let agents: Vec<super::Subagent> = agent["options"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|o| o["value"] != "default")
        .filter_map(|o| {
            Some(super::Subagent {
                name: o["value"].as_str()?.to_string(),
                description: o["description"].as_str().unwrap_or_default().to_string(),
            })
        })
        .collect();
    hub.save_subagents(&agents);
}

/// tab's `initialize`: no file system or terminal of its own (agents use
/// theirs, under the sandbox), and forms, which Claude Code's
/// AskUserQuestion arrives as.
fn initialize_params() -> Value {
    json!({
        "protocolVersion": 1,
        "clientCapabilities": {
            "fs": { "readTextFile": false, "writeTextFile": false },
            "terminal": false,
            "elicitation": { "form": {} },
        },
        "clientInfo": { "name": "tab", "version": env!("CARGO_PKG_VERSION") },
    })
}

/// Picks the chat's model: through the session's `model` option, or the
/// older `session/set_model` (OpenCode), when the agent offers that model.
async fn set_model(conn: &Conn, r: &Value, model: &str) {
    let session = &r["sessionId"];
    let option = r["configOptions"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|o| o["category"] == "model");
    // The session's `model` option: tried even when the model isn't listed
    // as is, since agents resolve their own aliases (Claude Code takes
    // "haiku" or a full id).
    if let Some(o) = option {
        let set = conn
            .request_value(
                "session/set_config_option",
                json!({ "sessionId": session, "configId": o["id"], "value": model }),
            )
            .await;
        match set {
            Ok(_) => return,
            Err(e) => tracing::debug!(model, error = %e, "the agent didn't take this model"),
        }
    }
    let listed = r["models"]["availableModels"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|m| m["modelId"] == model);
    if listed {
        let _ = conn
            .request_value(
                "session/set_model",
                json!({ "sessionId": session, "modelId": model }),
            )
            .await;
    }
}

/// Starts the session in the chat's permission mode, which a loaded Claude
/// Code conversation would otherwise take from its transcript.
async fn set_mode(conn: &Conn, session: &str, mode: &str) {
    if let Err(e) = conn
        .request_value(
            "session/set_mode",
            json!({ "sessionId": session, "modeId": mode }),
        )
        .await
    {
        tracing::warn!(mode, error = %e, "the agent didn't take this permission mode");
    }
}

fn text_block(text: &str) -> acp::ContentBlock {
    acp::ContentBlock::Text(acp::TextContent::new(text))
}

fn cancelled_permission() -> Value {
    serde_json::to_value(acp::RequestPermissionResponse::new(
        acp::RequestPermissionOutcome::Cancelled,
    ))
    .unwrap_or_default()
}

/// Waits for `fut` (one request's answer) while handling what the agent
/// sends meanwhile. `None` when the turn was stopped first.
async fn drive<F>(
    conn: &Conn,
    incoming: &mut mpsc::UnboundedReceiver<Incoming>,
    events: &mut Translator,
    cancel: &CancellationToken,
    fut: F,
) -> anyhow::Result<Option<Result<Value, RpcError>>>
where
    F: std::future::Future<Output = Result<Value, RpcError>>,
{
    tokio::pin!(fut);
    let mut open_in = true;
    let deadline = tokio::time::sleep(START_TIMEOUT);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            r = &mut fut => {
                drain(conn, events, incoming)?;
                return Ok(Some(r));
            }
            m = incoming.recv(), if open_in => open_in = handle(conn, events, m)?,
            _ = cancel.cancelled() => return Ok(None),
            // Only the start has a deadline; a turn runs as long as it needs.
            _ = &mut deadline, if events.replaying => anyhow::bail!("it didn't answer in time"),
        }
    }
}

/// Handles what the agent sent before an answer. Answers and the rest
/// arrive on separate channels, so an answer can be picked first while
/// what came before it (Claude Code's result) still waits; one reader keeps
/// them in order, so it's all queued by then.
fn drain(
    conn: &Conn,
    events: &mut Translator,
    incoming: &mut mpsc::UnboundedReceiver<Incoming>,
) -> anyhow::Result<()> {
    while let Ok(m) = incoming.try_recv() {
        handle(conn, events, Some(m))?;
    }
    Ok(())
}

/// One message from the agent: an update for the chat, or a request.
/// `false` once the agent closed its side; answers that came before it are
/// with their requests already, and the rest fail as "the agent exited".
fn handle(conn: &Conn, events: &mut Translator, m: Option<Incoming>) -> anyhow::Result<bool> {
    match m {
        Some(Incoming::Notification { method, params }) => {
            if method == "_claude/sdkMessage"
                && let Some(p) = &mut events.raw
            {
                p.read(&params["message"]);
                if !p.leaked().is_empty() && events.leaked.is_none() {
                    events.leaked = Some(p.leaked().join(", "));
                }
            }
            if method == "session/update" {
                match serde_json::from_value::<acp::SessionNotification>(params) {
                    Ok(n) => events.update(&n.update),
                    Err(e) => tracing::debug!(error = %e, "unreadable session/update"),
                }
            }
            anyhow::ensure!(events.leaked.is_none(), "strict");
        }
        Some(Incoming::Request { id, method, params }) => {
            on_request(conn, events, id, &method, params);
        }
        None => return Ok(false),
    }
    Ok(true)
}

/// The turn itself: waits for the prompt's answer while handling what the
/// agent sends. With `steering`, a message the person sends meanwhile goes
/// to the running turn: through `_session/steering` (`true`), or as another
/// `session/prompt` on the same session, which an agent that steers that way
/// (OpenCode) reads at its next step. The turn ends once every message has
/// its answer, so nothing sent is left unanswered; one the agent couldn't
/// take runs as the next turn.
async fn prompt_turn<F>(
    conn: &Conn,
    incoming: &mut mpsc::UnboundedReceiver<Incoming>,
    events: &mut Translator,
    cancel: &CancellationToken,
    fut: F,
    steering: Option<(&mut mpsc::UnboundedReceiver<Steer>, bool)>,
    session: &str,
) -> anyhow::Result<Option<Result<Value, RpcError>>>
where
    F: std::future::Future<Output = Result<Value, RpcError>>,
{
    use futures_util::StreamExt;
    use futures_util::stream::FuturesUnordered;
    type Steered = std::pin::Pin<
        Box<dyn std::future::Future<Output = (String, Result<Value, RpcError>)> + Send>,
    >;
    let (mut steering, extension) = match steering {
        Some((rx, ext)) => (Some(rx), ext),
        None => (None, false),
    };
    tokio::pin!(fut);
    let mut steered: FuturesUnordered<Steered> = FuturesUnordered::new();
    let mut open_in = true;
    let mut answer: Option<Result<Value, RpcError>> = None;
    loop {
        if answer.is_some() && steered.is_empty() {
            drain(conn, events, incoming)?;
            return Ok(answer);
        }
        let open = answer.is_none() && steering.is_some();
        tokio::select! {
            r = &mut fut, if answer.is_none() => answer = Some(r),
            Some((text, r)) = steered.next(), if !steered.is_empty() => match r {
                Ok(Value::Object(o)) if extension => {
                    // `injected` joined the turn; anything else wasn't taken.
                    if o.get("outcome").and_then(Value::as_str) != Some("injected") {
                        events.later(text);
                    }
                }
                Ok(Value::Object(o)) => {
                    if let Some(u) = o.get("usage") {
                        events.more_usage.push(u.clone());
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "steering failed; the message runs next");
                    events.later(text);
                }
            },
            s = async { steering.as_mut().expect("open").recv().await }, if open => match s {
                // Claude Code's modes but `bypassPermissions`: approvals
                // belong to the person.
                Some(Steer::Mode(mode))
                    if events.kind == AgentKind::ClaudeCode
                        && !super::claude::MODES.contains(&mode.as_str()) => {}
                Some(Steer::Mode(mode)) => {
                    // Switches at once, as Shift+Tab does in Claude Code.
                    let set = conn.request_value(
                        "session/set_mode",
                        json!({ "sessionId": session, "modeId": mode }),
                    );
                    tokio::spawn(async move {
                        if let Err(e) = set.await {
                            tracing::warn!(error = %e, "switching the permission mode failed");
                        }
                    });
                }
                Some(steer @ (Steer::Message(_) | Steer::Reminder(_))) => {
                    let (Steer::Message(text) | Steer::Reminder(text)) = &steer else {
                        unreachable!()
                    };
                    let text = text.clone();
                    // The person's message is owed an answer; tab's reminders aren't.
                    if let (Some(p), Steer::Message(_)) = (&events.raw, &steer) {
                        p.steered(&text);
                    }
                    let blocks = vec![text_block(&text)];
                    let r = if extension {
                        conn.request_value(
                            "_session/steering",
                            json!({
                                "sessionId": session,
                                "prompt": blocks,
                                // No turn to join: hand it back, and it runs next.
                                "_meta": { "steering": { "idleBehavior": "promptRequired" } },
                            }),
                        )
                    } else {
                        conn.request_value(
                            "session/prompt",
                            serde_json::to_value(acp::PromptRequest::new(session.to_string(), blocks))
                                .unwrap_or_default(),
                        )
                    };
                    steered.push(Box::pin(async move { (text, r.await) }));
                }
                // Nothing in ACP stops one tool and keeps the turn; a steer
                // during one moves it to the background (claude-agent-acp).
                Some(Steer::Interrupt) => {}
                None => steering = None,
            },
            m = incoming.recv(), if open_in => open_in = handle(conn, events, m)?,
            _ = cancel.cancelled() => return Ok(None),
        }
    }
}

/// A request from the agent. Permissions go to the person; tab offers no
/// file system or terminal (the agent uses its own, under the sandbox).
fn on_request(conn: &Conn, events: &Translator, id: Value, method: &str, params: Value) {
    if method == "elicitation/create" {
        let (hub, sid, conn) = (events.hub.clone(), events.session_id.clone(), conn.clone());
        tokio::spawn(async move {
            let answer = match ask_user_question(&params) {
                Some(input) => {
                    let summary = params["message"]
                        .as_str()
                        .unwrap_or("A question")
                        .to_string();
                    let d = hub
                        .request_approval(&sid, "AskUserQuestion", summary, input.clone())
                        .await;
                    form_answer(&input, &d)
                }
                // Forms tab can't show (an MCP server's own) are declined.
                None => json!({ "action": "decline" }),
            };
            conn.respond(id, Ok(answer));
        });
        return;
    }
    if method != "session/request_permission" {
        conn.respond(
            id,
            Err(RpcError {
                code: -32601,
                message: format!("tab doesn't offer {method}"),
            }),
        );
        return;
    }
    let req: acp::RequestPermissionRequest = match serde_json::from_value(params) {
        Ok(r) => r,
        Err(e) => {
            conn.respond(
                id,
                Err(RpcError {
                    code: -32602,
                    message: e.to_string(),
                }),
            );
            return;
        }
    };
    let hub = events.hub.clone();
    let sid = events.session_id.clone();
    let conn = conn.clone();
    let tool = events
        .tools
        .get(req.tool_call.tool_call_id.0.as_ref())
        .cloned();
    tokio::spawn(async move {
        let f = &req.tool_call.fields;
        let name = f
            .name
            .clone()
            .or_else(|| tool.as_ref().map(|t| t.name.clone()))
            .unwrap_or_else(|| "tool".into());
        let summary = f
            .title
            .clone()
            .or_else(|| tool.as_ref().map(|t| t.title.clone()))
            .unwrap_or_else(|| name.clone());
        let input = f
            .raw_input
            .clone()
            .or_else(|| tool.map(|t| t.input))
            .unwrap_or(Value::Null);
        let d = hub.request_approval(&sid, &name, summary, input).await;
        conn.respond(id, Ok(permission_answer(&req.options, &d)));
    });
}

/// Claude Code's AskUserQuestion, from the form claude-agent-acp turns it
/// into (`question_<n>` with its options, `question_<n>_custom` for one's
/// own answer): back to the tool's input, which tab's question card shows.
fn ask_user_question(params: &Value) -> Option<Value> {
    let props = params["requestedSchema"]["properties"].as_object()?;
    let single = !props.contains_key("question_1");
    let mut questions = Vec::new();
    for n in 0.. {
        let Some(q) = props.get(&format!("question_{n}")) else {
            break;
        };
        let multi = q["type"] == "array";
        let opts = if multi {
            &q["items"]["anyOf"]
        } else {
            &q["oneOf"]
        };
        let options: Vec<Value> = opts
            .as_array()?
            .iter()
            .map(|o| json!({ "label": o["const"], "description": o["description"] }))
            .collect();
        let text = if single {
            params["message"].clone()
        } else {
            q["description"].clone()
        };
        questions.push(json!({
            "question": text,
            "header": q["title"],
            "options": options,
            "multiSelect": multi,
        }));
    }
    (!questions.is_empty()).then(|| json!({ "questions": questions }))
}

/// The person's answers (by question text, as tab's card gives them) as
/// the form's content: a pick goes in its field, anything else in the
/// question's own-answer field.
fn form_answer(input: &Value, d: &Decision) -> Value {
    let Decision::Answer(answers) = d else {
        return json!({ "action": "decline" });
    };
    let mut content = serde_json::Map::new();
    for (n, q) in input["questions"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        let Some(a) = q["question"].as_str().and_then(|t| answers[t].as_str()) else {
            continue;
        };
        let labels: Vec<&str> = q["options"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|o| o["label"].as_str())
            .collect();
        if q["multiSelect"] == true {
            let (picked, own): (Vec<&str>, Vec<&str>) =
                a.split(", ").partition(|p| labels.contains(p));
            content.insert(format!("question_{n}"), json!(picked));
            if !own.is_empty() {
                content.insert(format!("question_{n}_custom"), json!(own.join(", ")));
            }
        } else if labels.contains(&a) {
            content.insert(format!("question_{n}"), json!(a));
        } else {
            content.insert(format!("question_{n}_custom"), json!(a));
        }
    }
    json!({ "action": "accept", "content": content })
}

/// The option that matches the person's decision.
fn permission_answer(options: &[acp::PermissionOption], d: &Decision) -> Value {
    use acp::PermissionOptionKind as K;
    let wanted: &[K] = match d {
        Decision::Allow | Decision::Answer(_) => &[K::AllowOnce, K::AllowAlways],
        Decision::AllowAlways => &[K::AllowAlways, K::AllowOnce],
        Decision::Deny(_) => &[K::RejectOnce, K::RejectAlways],
    };
    let pick = wanted
        .iter()
        .find_map(|k| options.iter().find(|o| o.kind == *k));
    let outcome = match pick {
        Some(o) => acp::RequestPermissionOutcome::Selected(acp::SelectedPermissionOutcome::new(
            o.option_id.clone(),
        )),
        None => acp::RequestPermissionOutcome::Cancelled,
    };
    serde_json::to_value(acp::RequestPermissionResponse::new(outcome)).unwrap_or_default()
}

#[derive(Debug)]
struct RpcError {
    code: i64,
    message: String,
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.message, self.code)
    }
}

impl std::error::Error for RpcError {}

enum Incoming {
    Notification {
        method: String,
        params: Value,
    },
    Request {
        id: Value,
        method: String,
        params: Value,
    },
}

/// JSON-RPC 2.0 over the agent's stdin and stdout, one message per line.
type Waiters = Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value, RpcError>>>>>;

#[derive(Clone)]
struct Conn {
    out: mpsc::UnboundedSender<String>,
    next: Arc<AtomicI64>,
    pending: Waiters,
}

impl Conn {
    fn start(
        mut stdin: impl tokio::io::AsyncWrite + Unpin + Send + 'static,
        stdout: impl tokio::io::AsyncRead + Unpin + Send + 'static,
    ) -> (Self, mpsc::UnboundedReceiver<Incoming>) {
        let (out, mut lines) = mpsc::unbounded_channel::<String>();
        tokio::spawn(async move {
            while let Some(l) = lines.recv().await {
                if stdin.write_all(l.as_bytes()).await.is_err()
                    || stdin.write_all(b"\n").await.is_err()
                    || stdin.flush().await.is_err()
                {
                    break;
                }
            }
        });
        let pending: Arc<Mutex<HashMap<i64, oneshot::Sender<_>>>> = Arc::default();
        let (tx, rx) = mpsc::unbounded_channel();
        let waiting = pending.clone();
        tokio::spawn(async move {
            let mut r = BufReader::new(stdout).lines();
            while let Ok(Some(l)) = r.next_line().await {
                let Ok(msg) = serde_json::from_str::<Value>(l.trim()) else {
                    if !l.trim().is_empty() {
                        tracing::debug!(line = %l, "non-JSON output from an ACP agent");
                    }
                    continue;
                };
                let method = msg["method"].as_str().map(str::to_string);
                match (method, msg.get("id").cloned()) {
                    (None, Some(id)) => {
                        let Some(id) = id.as_i64() else { continue };
                        if let Some(w) = waiting.lock().await.remove(&id) {
                            let _ = w.send(match msg.get("error") {
                                Some(e) if !e.is_null() => Err(RpcError {
                                    code: e["code"].as_i64().unwrap_or(0),
                                    message: e["message"]
                                        .as_str()
                                        .unwrap_or("the agent reported an error")
                                        .to_string(),
                                }),
                                _ => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
                            });
                        }
                    }
                    (Some(method), Some(id)) => {
                        let params = msg.get("params").cloned().unwrap_or(Value::Null);
                        let _ = tx.send(Incoming::Request { id, method, params });
                    }
                    (Some(method), None) => {
                        let params = msg.get("params").cloned().unwrap_or(Value::Null);
                        let _ = tx.send(Incoming::Notification { method, params });
                    }
                    (None, None) => {}
                }
            }
            // The agent is gone: fail whatever still waits.
            waiting.lock().await.clear();
        });
        (
            Self {
                out,
                next: Arc::new(AtomicI64::new(0)),
                pending,
            },
            rx,
        )
    }

    fn request<P: serde::Serialize>(
        &self,
        method: &str,
        params: P,
    ) -> impl std::future::Future<Output = Result<Value, RpcError>> + use<P> {
        let params = serde_json::to_value(params).unwrap_or(Value::Null);
        self.request_value(method, params)
    }

    fn request_value(
        &self,
        method: &str,
        params: Value,
    ) -> impl std::future::Future<Output = Result<Value, RpcError>> + use<> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let (pending, out) = (self.pending.clone(), self.out.clone());
        async move {
            let (tx, rx) = oneshot::channel();
            pending.lock().await.insert(id, tx);
            let gone = || RpcError {
                code: -32000,
                message: "the agent exited".into(),
            };
            out.send(msg.to_string()).map_err(|_| gone())?;
            rx.await.map_err(|_| gone())?
        }
    }

    fn notify(&self, method: &str, params: impl serde::Serialize) {
        let params = serde_json::to_value(params).unwrap_or(Value::Null);
        let msg = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        let _ = self.out.send(msg.to_string());
    }

    fn respond(&self, id: Value, result: Result<Value, RpcError>) {
        let msg = match result {
            Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
            Err(e) => {
                json!({ "jsonrpc": "2.0", "id": id, "error": { "code": e.code, "message": e.message } })
            }
        };
        let _ = self.out.send(msg.to_string());
    }
}

#[derive(Clone)]
struct Tool {
    name: String,
    title: String,
    input: Value,
    creates: bool,
    /// A command's output, as it streamed (codex-acp's
    /// `_meta.terminal_output_delta`).
    output: String,
}

/// Turns `session/update` into the chat: deltas while the agent writes, a
/// message when it finishes one, a tool card per call and its result.
struct Translator {
    hub: Arc<AgentHub>,
    session_id: String,
    kind: AgentKind,
    /// The agent message being written.
    text: String,
    tools: HashMap<String, Tool>,
    /// The session's total cost, as the agent last reported it this turn.
    cost: Option<f64>,
    /// Its total before this turn: from the last turn (`cost_file`), or
    /// what the agent reported while loading the session.
    cost_before: f64,
    cost_file: Option<std::path::PathBuf>,
    /// The turn's tokens, if the agent reports them (`PromptResponse.usage`).
    usage: Option<Value>,
    /// Tokens of the prompts sent mid-turn.
    more_usage: Vec<Value>,
    model: Option<String>,
    /// Updates that replay a loaded session's history.
    replaying: bool,
    /// The agent wrote text or called a tool this turn.
    said_anything: bool,
    finished: bool,
    strict: bool,
    /// How the agent names MCP tools (`mcp__{server}__{tool}` or
    /// `{server}_{tool}`), to spot servers tab didn't add.
    mcp_tool_name: Option<String>,
    /// MCP servers tab added.
    allowed: Vec<String>,
    /// The person's servers strict mode turned off.
    foreign: Vec<String>,
    leaked: Option<String>,
    /// Claude Code: its own messages, which tab reads the chat from (with
    /// everything tab's Claude Code reader does), instead of the updates.
    raw: Option<super::claude::Parser>,
}

impl Translator {
    fn new(hub: Arc<AgentHub>, session_id: String, kind: AgentKind) -> Self {
        Self {
            hub,
            session_id,
            kind,
            text: String::new(),
            tools: HashMap::new(),
            cost: None,
            cost_before: 0.0,
            cost_file: None,
            usage: None,
            more_usage: Vec::new(),
            model: None,
            replaying: false,
            said_anything: false,
            finished: false,
            strict: false,
            mcp_tool_name: None,
            allowed: Vec::new(),
            foreign: Vec::new(),
            leaked: None,
            raw: None,
        }
    }

    fn update(&mut self, u: &acp::SessionUpdate) {
        if let acp::SessionUpdate::AvailableCommandsUpdate(c) = u {
            self.commands(&c.available_commands);
            return;
        }
        if let acp::SessionUpdate::UsageUpdate(u) = u {
            // The session's running total, in dollars.
            if let Some(c) = u.cost.as_ref().filter(|c| c.currency == "USD") {
                if self.replaying {
                    self.cost_before = c.amount;
                } else {
                    self.cost = Some(c.amount);
                }
            }
            return;
        }
        if self.replaying || self.raw.is_some() {
            return;
        }
        let sid = self.session_id.clone();
        match u {
            acp::SessionUpdate::AgentMessageChunk(c) => {
                if let acp::ContentBlock::Text(t) = &c.content {
                    self.said_anything |= !t.text.trim().is_empty();
                    self.hub.delta(&sid, &t.text);
                    self.text.push_str(&t.text);
                }
            }
            acp::SessionUpdate::ToolCall(tc) => {
                self.said_anything = true;
                self.flush();
                let id = tc.tool_call_id.0.to_string();
                let name = tc
                    .name
                    .clone()
                    .unwrap_or_else(|| kind_name(&tc.kind).to_string());
                let input = tc.raw_input.clone().unwrap_or(Value::Null);
                self.check_strict(&name, &tc.title);
                let tool = Tool {
                    creates: crate::links::is_creator(&name, &input),
                    output: String::new(),
                    title: tc.title.clone(),
                    name,
                    input,
                };
                self.hub.post(
                    &sid,
                    "assistant",
                    "tool",
                    &tool.title,
                    json!({ "tool": tool.name, "id": id, "input": tool.input }),
                );
                self.tools.insert(id.clone(), tool);
                if matches!(
                    tc.status,
                    acp::ToolCallStatus::Completed | acp::ToolCallStatus::Failed
                ) {
                    self.tool_done(
                        &id,
                        tc.status == acp::ToolCallStatus::Failed,
                        &tc.content,
                        tc.raw_output.as_ref(),
                    );
                }
            }
            acp::SessionUpdate::ToolCallUpdate(tu) => {
                let id = tu.tool_call_id.0.to_string();
                let f = &tu.fields;
                if let Some(t) = self.tools.get_mut(&id) {
                    if let Some(m) = &tu.meta {
                        for k in ["terminal_output_delta", "terminal_output"] {
                            if let Some(d) = m.get(k).and_then(|o| o["data"].as_str()) {
                                t.output.push_str(d);
                            }
                        }
                    }
                    if let Some(input) = &f.raw_input {
                        t.input = input.clone();
                    }
                    if let Some(title) = &f.title {
                        t.title = title.clone();
                    }
                }
                if let Some(
                    status @ (acp::ToolCallStatus::Completed | acp::ToolCallStatus::Failed),
                ) = f.status
                {
                    self.tool_done(
                        &id,
                        status == acp::ToolCallStatus::Failed,
                        f.content.as_deref().unwrap_or_default(),
                        f.raw_output.as_ref(),
                    );
                }
            }
            _ => {}
        }
    }

    fn tool_done(
        &mut self,
        id: &str,
        failed: bool,
        content: &[acp::ToolCallContent],
        raw_output: Option<&Value>,
    ) {
        let Some(tool) = self.tools.remove(id) else {
            return;
        };
        let mut text = content
            .iter()
            .filter_map(|c| match c {
                acp::ToolCallContent::Content(c) => match &c.content {
                    acp::ContentBlock::Text(t) => Some(t.text.clone()),
                    _ => None,
                },
                acp::ToolCallContent::Diff(d) => Some(format!("Edited {}", d.path.display())),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        if text.is_empty() {
            text = tool.output.clone();
        }
        if text.is_empty()
            && let Some(o) = raw_output
        {
            text = match o {
                Value::String(s) => s.clone(),
                o => o["output"]
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| o.to_string()),
            };
        }
        if tool.creates {
            self.hub.attach_created_links(&self.session_id, &text);
        }
        self.hub.post(
            &self.session_id,
            "tool",
            "tool_result",
            &text.chars().take(4000).collect::<String>(),
            json!({ "tool_use_id": id, "is_error": failed }),
        );
    }

    /// Strict mode: a tool from an MCP server tab didn't add.
    fn check_strict(&mut self, name: &str, title: &str) {
        if !self.strict || self.leaked.is_some() {
            return;
        }
        for n in [name, title] {
            if let Some(server) = foreign_server(
                n,
                self.mcp_tool_name.as_deref(),
                &self.allowed,
                &self.foreign,
            ) {
                self.leaked = Some(format!("the MCP server {server}"));
                return;
            }
        }
    }

    fn commands(&self, cmds: &[acp::AvailableCommand]) {
        let cmds: Vec<SlashCommand> = cmds
            .iter()
            .map(|c| SlashCommand {
                name: c.name.clone(),
                description: c.description.clone(),
                argument_hint: match &c.input {
                    Some(acp::AvailableCommandInput::Unstructured(i)) => i.hint.clone(),
                    _ => String::new(),
                },
                aliases: Vec::new(),
                kind: match &self.raw {
                    _ if c.name.starts_with("mcp:") => SlashKind::Mcp,
                    Some(p) => p.command_kind(&c.name, &c.description),
                    None => SlashKind::Command,
                },
            })
            .collect();
        self.hub.save_slash_commands(self.kind, &cmds);
    }

    fn flush(&mut self) {
        let text = std::mem::take(&mut self.text);
        if !text.trim().is_empty() {
            self.hub
                .post(&self.session_id, "assistant", "text", &text, Value::Null);
        }
    }

    /// A message the agent couldn't take mid-turn: it runs as the next turn.
    fn later(&self, text: String) {
        self.hub
            .followups
            .lock()
            .unwrap()
            .entry(self.session_id.clone())
            .or_default()
            .push(text);
    }

    /// What this turn cost: the change in the session's total.
    fn turn_cost(&self) -> f64 {
        self.cost
            .map(|c| (c - self.cost_before).max(0.0))
            .unwrap_or(0.0)
    }

    /// Keeps the turn's cost and tokens for the usage page, and the
    /// session's total for the next turn's baseline.
    fn record_usage(&self) {
        if let (Some(total), Some(f)) = (self.cost, &self.cost_file) {
            let _ = std::fs::write(f, total.to_string());
        }
        let n = |k: &str| {
            self.usage
                .iter()
                .chain(&self.more_usage)
                .filter_map(|u| u[k].as_i64())
                .sum::<i64>()
        };
        if self.cost.is_none() && self.usage.is_none() {
            return;
        }
        let u = crate::store::NewUsage {
            purpose: "agent",
            session_id: Some(self.session_id.clone()),
            agent: self.kind.as_str().into(),
            model: self.model.clone(),
            billing: "api",
            cost_usd: self.turn_cost(),
            input_tokens: n("inputTokens") + n("cachedReadTokens") + n("cachedWriteTokens"),
            output_tokens: n("outputTokens") + n("thoughtTokens"),
        };
        if let Err(e) = self.hub.store.usage_add(&u, chrono::Utc::now()) {
            tracing::warn!(error = %e, "couldn't store usage");
        }
    }

    fn finish(&mut self, stop: Result<acp::StopReason, String>) {
        if std::mem::replace(&mut self.finished, true) {
            return;
        }
        // Claude Code's own result said how the turn went, with its usage.
        if let Some(p) = &self.raw {
            if !p.finished()
                && let Err(e) = &stop
            {
                self.hub
                    .post(&self.session_id, "system", "error", e, Value::Null);
                self.hub
                    .update(&self.session_id, Some(SessionStatus::Failed), None);
            }
            return;
        }
        self.record_usage();
        self.flush();
        let sid = self.session_id.clone();
        let error = match stop {
            Ok(acp::StopReason::EndTurn | acp::StopReason::Cancelled) => None,
            Ok(acp::StopReason::MaxTokens) => Some("The model hit its output limit.".to_string()),
            Ok(acp::StopReason::MaxTurnRequests) => {
                Some("The agent hit its limit of model requests for one turn.".to_string())
            }
            Ok(acp::StopReason::Refusal) => Some("The model refused to continue.".to_string()),
            Ok(_) => None,
            Err(e) => Some(e),
        };
        if let Some(e) = &error {
            self.hub.post(&sid, "system", "error", e, Value::Null);
        }
        self.hub.post(
            &sid,
            "system",
            "result",
            "",
            json!({ "cost_usd": self.turn_cost() }),
        );
        self.hub.update(
            &sid,
            Some(if error.is_some() {
                SessionStatus::Failed
            } else {
                SessionStatus::Idle
            }),
            None,
        );
    }
}

fn kind_name(k: &acp::ToolKind) -> &'static str {
    match k {
        acp::ToolKind::Read => "read",
        acp::ToolKind::Edit => "edit",
        acp::ToolKind::Delete => "delete",
        acp::ToolKind::Move => "move",
        acp::ToolKind::Search => "search",
        acp::ToolKind::Execute => "execute",
        acp::ToolKind::Think => "think",
        acp::ToolKind::Fetch => "fetch",
        _ => "tool",
    }
}

/// The MCP server a tool named `name` comes from, if it's one tab didn't
/// add. `format` is how the agent names MCP tools: `mcp__{server}__{tool}`
/// and `mcp.{server}.{tool}` name the server outright; `{server}_{tool}`
/// can only be matched against the person's own servers.
fn foreign_server(
    name: &str,
    format: Option<&str>,
    allowed: &[String],
    foreign: &[String],
) -> Option<String> {
    let named = match format {
        Some("mcp.{server}.{tool}") => name.strip_prefix("mcp.").map(|r| (r, ".")),
        _ => None,
    }
    .or_else(|| name.strip_prefix("mcp__").map(|r| (r, "__")));
    if let Some((rest, sep)) = named {
        let server = rest.split(sep).next().unwrap_or(rest);
        return (!allowed.iter().any(|a| a == server)).then(|| server.to_string());
    }
    if format == Some("mcp.{server}.{tool}")
        && let Some(rest) = name.strip_prefix("mcp.")
        && let Some((server, _)) = rest.split_once('.')
    {
        return (!allowed.iter().any(|a| a == server)).then(|| server.to_string());
    }
    if format == Some("{server}_{tool}") {
        return foreign
            .iter()
            .find(|f| name.starts_with(&format!("{f}_")))
            .cloned();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AgentConfig;
    use crate::store::Store;
    use chrono::Utc;

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
            .session_create(None, AgentKind::OpenCode, "t", ".", Utc::now())
            .unwrap();
        (hub, store, s)
    }

    fn update(t: &mut Translator, v: Value) {
        let n: acp::SessionNotification =
            serde_json::from_value(json!({ "sessionId": "s1", "update": v })).unwrap();
        t.update(&n.update);
    }

    #[tokio::test]
    async fn updates_become_the_chat() {
        let (hub, store, s) = hub_and_session("tab-acp-test");
        let mut t = Translator::new(hub, s.id.clone(), AgentKind::OpenCode);
        update(
            &mut t,
            json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "Look" } }),
        );
        update(
            &mut t,
            json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "ing." } }),
        );
        update(
            &mut t,
            json!({ "sessionUpdate": "tool_call", "toolCallId": "c1", "title": "ls", "kind": "execute", "status": "pending", "rawInput": { "command": "ls" } }),
        );
        update(
            &mut t,
            json!({ "sessionUpdate": "tool_call_update", "toolCallId": "c1", "status": "completed", "content": [{ "type": "content", "content": { "type": "text", "text": "README.md" } }] }),
        );
        update(
            &mut t,
            json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "One file." } }),
        );
        update(
            &mut t,
            json!({ "sessionUpdate": "usage_update", "used": 100, "size": 1000, "cost": { "amount": 0.003, "currency": "USD" } }),
        );
        t.finish(Ok(acp::StopReason::EndTurn));
        let m = store.messages(&s.id).unwrap();
        let kinds: Vec<_> = m.iter().map(|m| m.kind.as_str()).collect();
        assert_eq!(kinds, ["text", "tool", "tool_result", "text", "result"]);
        assert_eq!(m[0].content, "Looking.");
        assert_eq!(m[1].content, "ls");
        assert_eq!(m[1].meta["tool"], "execute");
        assert_eq!(m[2].content, "README.md");
        assert!((m[4].meta["cost_usd"].as_f64().unwrap() - 0.003).abs() < 1e-9);
        assert_eq!(
            store.session_get(&s.id).unwrap().unwrap().status,
            SessionStatus::Idle
        );
    }

    #[tokio::test]
    async fn a_commands_streamed_output_is_its_result() {
        let (hub, store, s) = hub_and_session("tab-acp-test-output");
        let mut t = Translator::new(hub, s.id.clone(), AgentKind::Codex);
        update(
            &mut t,
            json!({ "sessionUpdate": "tool_call", "toolCallId": "c1", "title": "cat notes.txt", "kind": "execute", "status": "in_progress" }),
        );
        for chunk in ["the code ", "word is pelican\n"] {
            update(
                &mut t,
                json!({ "sessionUpdate": "tool_call_update", "toolCallId": "c1", "_meta": { "terminal_output_delta": { "data": chunk, "terminal_id": "c1" } } }),
            );
        }
        update(
            &mut t,
            json!({ "sessionUpdate": "tool_call_update", "toolCallId": "c1", "status": "completed", "rawOutput": { "exit_code": 0 } }),
        );
        let m = store.messages(&s.id).unwrap();
        assert_eq!(m.last().unwrap().content, "the code word is pelican\n");
    }

    #[tokio::test]
    async fn a_loaded_sessions_history_isnt_posted_again() {
        let (hub, store, s) = hub_and_session("tab-acp-test-2");
        let mut t = Translator::new(hub, s.id.clone(), AgentKind::OpenCode);
        t.replaying = true;
        update(
            &mut t,
            json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "old" } }),
        );
        t.replaying = false;
        t.flush();
        assert!(store.messages(&s.id).unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_turn_costs_what_the_session_total_rose_by() {
        let (hub, store, s) = hub_and_session("tab-acp-test-cost");
        let mut t = Translator::new(hub, s.id.clone(), AgentKind::OpenCode);
        let file = std::env::temp_dir().join(format!("tab-acp-cost-{}", s.id));
        t.cost_file = Some(file.clone());
        // Loading the session reports what it had cost so far.
        t.replaying = true;
        update(
            &mut t,
            json!({ "sessionUpdate": "usage_update", "used": 1, "size": 9, "cost": { "amount": 1.25, "currency": "USD" } }),
        );
        t.replaying = false;
        update(
            &mut t,
            json!({ "sessionUpdate": "usage_update", "used": 2, "size": 9, "cost": { "amount": 1.5, "currency": "USD" } }),
        );
        t.usage = Some(
            json!({ "totalTokens": 30, "inputTokens": 10, "outputTokens": 5, "cachedReadTokens": 15 }),
        );
        t.finish(Ok(acp::StopReason::EndTurn));
        let m = store.messages(&s.id).unwrap();
        let result = m.iter().find(|m| m.kind == "result").unwrap();
        assert!((result.meta["cost_usd"].as_f64().unwrap() - 0.25).abs() < 1e-9);
        // The next turn starts from this total.
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "1.5");
        let _ = std::fs::remove_file(file);
    }

    /// Against a real agent with a signed-in provider, e.g.
    /// `TAB_ACP_LIVE=codex TAB_ACP_LIVE_MODEL=gpt-5.2 cargo test -p tab-core live_acp -- --ignored --nocapture`
    /// (`TAB_ACP_LIVE` is a harness id; the model is optional).
    #[tokio::test]
    #[ignore]
    async fn live_acp() {
        let Ok(kind) = std::env::var("TAB_ACP_LIVE") else {
            return;
        };
        let kind = AgentKind::parse(&kind);
        let model = std::env::var("TAB_ACP_LIVE_MODEL").ok();
        let (hub, store, s) = hub_and_session("tab-acp-live");
        let answer = complete(
            &hub,
            kind,
            "Answer in one word.",
            "What colour is a clear daytime sky?",
            model.as_deref(),
        )
        .await
        .unwrap();
        println!("complete: {answer}");
        assert!(answer.to_lowercase().contains("blue"), "{answer}");

        let mut cfg = AgentConfig::default();
        cfg.set_model(kind, model);
        // `TAB_ACP_LIVE_STRICT=1` runs the chat strict.
        cfg.strict = std::env::var("TAB_ACP_LIVE_STRICT").is_ok();
        hub.set_config(cfg).await;
        let dir = std::env::temp_dir().join(format!("tab-acp-live-{}", s.id));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("notes.txt"), "the code word is pelican\n").unwrap();
        let s = store
            .session_create(None, kind, "t", &dir.to_string_lossy(), Utc::now())
            .unwrap();
        // Nobody is at the keyboard: allow whatever the agent asks, and say what it was.
        let approver = {
            let (hub, sid) = (hub.clone(), s.id.clone());
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    for a in hub.pending_approvals(&sid) {
                        println!("approval: {} · {}", a.tool, a.summary);
                        hub.resolve_approval(&a.id, Decision::Allow);
                    }
                }
            })
        };
        // Where the agent can't keep its conversation between processes (a
        // sandboxed test run), `TAB_ACP_LIVE_TURNS=1` stops after one.
        let turns: usize = std::env::var("TAB_ACP_LIVE_TURNS")
            .ok()
            .and_then(|n| n.parse().ok())
            .unwrap_or(2);
        for prompt in [
            "Read notes.txt and tell me the code word.",
            "What was the code word again? Answer from memory.",
        ]
        .into_iter()
        .take(turns)
        {
            let s = store.session_get(&s.id).unwrap().unwrap();
            run_turn(
                &hub,
                &s,
                prompt,
                "You are being tested.",
                CancellationToken::new(),
            )
            .await
            .unwrap();
        }
        let m = store.messages(&s.id).unwrap();
        println!(
            "agent session: {:?}",
            store.session_get(&s.id).unwrap().unwrap().agent_session_id
        );
        for x in &m {
            if !x.meta.is_null() {
                println!(
                    "  meta: {}",
                    x.meta.to_string().chars().take(200).collect::<String>()
                );
            }
            println!(
                "{}/{}: {}",
                x.role,
                x.kind,
                x.content.chars().take(120).collect::<String>()
            );
        }
        let texts: Vec<_> = m.iter().filter(|m| m.kind == "text").collect();
        assert!(texts.len() >= turns, "a reply per question");
        // The answer to each question, after any preamble.
        let answers: Vec<_> = m
            .iter()
            .filter(|m| m.kind == "result")
            .map(|r| m.iter().rfind(|t| t.kind == "text" && t.id < r.id).unwrap())
            .collect();
        assert_eq!(answers.len(), turns);
        assert!(
            answers
                .iter()
                .all(|t| t.content.to_lowercase().contains("pelican"))
        );
        assert!(m.iter().any(|m| m.kind == "tool"), "it read the file");
        approver.abort();
        let _ = std::fs::remove_dir_all(dir);
    }

    /// tab's rewind with a real Claude Code: two turns, back to before the
    /// second, and the agent remembers only the first. Run from a terminal
    /// (it keeps transcripts in `~/.claude`), with `claude-agent-acp` on PATH:
    /// `TAB_ACP_LIVE_REWIND=1 cargo test -p tab-core live_claude_rewind -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn live_claude_rewind() {
        if std::env::var("TAB_ACP_LIVE_REWIND").is_err() {
            return;
        }
        let (hub, store, _) = hub_and_session("tab-acp-live-rewind");
        let mut cfg = AgentConfig::default();
        cfg.set_model(AgentKind::ClaudeCode, Some("haiku".into()));
        hub.set_config(cfg).await;
        let dir = std::env::temp_dir().join(format!("tab-acp-rewind-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let s = store
            .session_create(
                None,
                AgentKind::ClaudeCode,
                "t",
                &dir.to_string_lossy(),
                Utc::now(),
            )
            .unwrap();
        let turn = |prompt: &'static str| {
            let (hub, store, sid) = (hub.clone(), store.clone(), s.id.clone());
            async move {
                hub.post(&sid, "user", "text", prompt, Value::Null);
                let s = store.session_get(&sid).unwrap().unwrap();
                run_turn(
                    &hub,
                    &s,
                    prompt,
                    "You are being tested.",
                    CancellationToken::new(),
                )
                .await
                .unwrap();
                let m = store.messages(&sid).unwrap();
                let reply = m
                    .iter()
                    .rev()
                    .find(|m| m.role == "assistant" && m.kind == "text");
                reply.map(|m| m.content.clone()).unwrap_or_default()
            }
        };
        println!(
            "1: {}",
            turn("Remember the word APPLE. Reply with only OK.").await
        );
        let id = store.session_get(&s.id).unwrap().unwrap().agent_session_id;
        println!(
            "2: {}",
            turn("Remember the word BANANA. Reply with only OK.").await
        );
        assert_eq!(
            store.session_get(&s.id).unwrap().unwrap().agent_session_id,
            id,
            "one conversation"
        );
        let second = store
            .messages(&s.id)
            .unwrap()
            .into_iter()
            .filter(|m| m.role == "user" && m.kind == "text")
            .nth(1)
            .unwrap();
        println!("meta: {}", second.meta);
        hub.rewind(&s.id, second.id, true, false).await.unwrap();
        let ask = "List every word I asked you to remember, comma separated, nothing else.";
        let after = turn(ask).await;
        println!("3: {after}");
        assert!(
            after.contains("APPLE") && !after.contains("BANANA"),
            "{after}"
        );
        // And it stays rewound.
        let again = turn(ask).await;
        println!("4: {again}");
        assert!(!again.contains("BANANA"), "{again}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_turn_with_nothing_in_it_is_a_failure() {
        let (hub, _store, s) = hub_and_session("tab-acp-test-empty");
        let mut t = Translator::new(hub, s.id.clone(), AgentKind::OpenCode);
        t.usage = Some(json!({ "totalTokens": 0, "inputTokens": 0, "outputTokens": 0 }));
        assert!(empty_turn(&t, &acp::StopReason::EndTurn));
        assert!(!empty_turn(&t, &acp::StopReason::Cancelled));
        update(
            &mut t,
            json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "Hi" } }),
        );
        assert!(!empty_turn(&t, &acp::StopReason::EndTurn));
    }

    /// The person sends a message mid-turn: it reaches the agent as a second
    /// `session/prompt`, and the turn ends only once both are answered.
    #[tokio::test]
    async fn a_message_mid_turn_steers_the_running_turn() {
        let (hub, store, s) = hub_and_session("tab-acp-test-steer");
        let (client_io, agent_io) = tokio::io::duplex(1 << 16);
        let (cr, cw) = tokio::io::split(client_io);
        let (conn, mut incoming) = Conn::start(cw, cr);
        let (ar, mut aw) = tokio::io::split(agent_io);
        let (steer_tx, mut steer_rx) = mpsc::unbounded_channel();
        let agent = tokio::spawn(async move {
            let mut lines = BufReader::new(ar).lines();
            let mut next = async || -> Value {
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap()
            };
            let first = next().await;
            assert_eq!(first["params"]["prompt"][0]["text"], "fix the bug");
            // Working on it; the person steers now.
            let chunk = json!({ "jsonrpc": "2.0", "method": "session/update", "params": { "sessionId": "s1", "update": { "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "Looking." } } } });
            aw.write_all(format!("{chunk}\n").as_bytes()).await.unwrap();
            steer_tx
                .send(Steer::Message("use the other file".into()))
                .unwrap();
            let second = next().await;
            assert_eq!(second["method"], "session/prompt");
            assert_eq!(second["params"]["sessionId"], "s1");
            assert_eq!(second["params"]["prompt"][0]["text"], "use the other file");
            // Both end with the loop, the steered one last.
            for (id, out) in [(&first["id"], 5), (&second["id"], 7)] {
                let r = json!({ "jsonrpc": "2.0", "id": id, "result": { "stopReason": "end_turn", "usage": { "totalTokens": out, "inputTokens": 0, "outputTokens": out } } });
                aw.write_all(format!("{r}\n").as_bytes()).await.unwrap();
            }
        });
        let mut t = Translator::new(hub, s.id.clone(), AgentKind::OpenCode);
        let r = prompt_turn(
            &conn,
            &mut incoming,
            &mut t,
            &CancellationToken::new(),
            conn.request(
                "session/prompt",
                acp::PromptRequest::new("s1", vec![text_block("fix the bug")]),
            ),
            Some((&mut steer_rx, false)),
            "s1",
        )
        .await
        .unwrap()
        .unwrap()
        .unwrap();
        agent.await.unwrap();
        assert_eq!(r["usage"]["outputTokens"], 5);
        // The steered prompt's tokens count too.
        assert_eq!(t.more_usage.len(), 1);
        t.usage = Some(r["usage"].clone());
        t.finish(Ok(acp::StopReason::EndTurn));
        let m = store.messages(&s.id).unwrap();
        assert!(
            m.iter()
                .any(|m| m.kind == "text" && m.content == "Looking.")
        );
    }

    #[test]
    fn every_harness_but_pi_runs_over_acp() {
        let cfg = AgentConfig::default();
        for strict in [false, true] {
            assert!(enabled(&cfg, AgentKind::OpenCode, strict));
            assert!(enabled(&cfg, AgentKind::Codex, strict));
            assert!(enabled(&cfg, AgentKind::ClaudeCode, strict));
            assert!(!enabled(&cfg, AgentKind::Pi, strict));
        }
    }

    #[tokio::test]
    async fn a_refusal_fails_the_turn() {
        let (hub, store, s) = hub_and_session("tab-acp-test-3");
        let mut t = Translator::new(hub, s.id.clone(), AgentKind::OpenCode);
        t.finish(Ok(acp::StopReason::Refusal));
        assert_eq!(
            store.session_get(&s.id).unwrap().unwrap().status,
            SessionStatus::Failed
        );
    }

    #[test]
    fn strict_spots_servers_tab_didnt_add() {
        let allowed = vec!["tab".to_string(), "linear".to_string()];
        let foreign = vec!["github".to_string()];
        let f = |n| foreign_server(n, Some("{server}_{tool}"), &allowed, &foreign);
        assert_eq!(f("mcp__tab__tab_search"), None);
        assert_eq!(f("mcp__linear__issues"), None);
        assert_eq!(f("mcp__slack__post"), Some("slack".into()));
        assert_eq!(f("github_create_issue"), Some("github".into()));
        assert_eq!(f("bash"), None);
        // codex-acp's titles.
        let codex = |n| foreign_server(n, Some("mcp.{server}.{tool}"), &allowed, &[]);
        assert_eq!(codex("mcp.tab.tab_search"), None);
        assert_eq!(codex("mcp.slack.post"), Some("slack".into()));
        assert_eq!(codex("mcp__slack__post"), Some("slack".into()));
        assert_eq!(codex("execute"), None);
        assert_eq!(
            foreign_server("github_create_issue", None, &allowed, &foreign),
            None
        );
        // codex-acp's titles.
        let codex = |n| foreign_server(n, Some("mcp.{server}.{tool}"), &allowed, &[]);
        assert_eq!(codex("mcp.tab.tab_search"), None);
        assert_eq!(codex("mcp.slack.post"), Some("slack".into()));
        assert_eq!(codex("ls -la"), None);
    }

    /// Claude Code's result comes just before the prompt's answer, on
    /// another channel: the turn still reads it before it ends.
    #[tokio::test]
    async fn what_came_before_the_answer_is_read_first() {
        let (hub, store, s) = hub_and_session("tab-acp-test-drain");
        let (client_io, _agent_io) = tokio::io::duplex(1 << 10);
        let (cr, cw) = tokio::io::split(client_io);
        let (conn, _) = Conn::start(cw, cr);
        let (tx, mut incoming) = mpsc::unbounded_channel();
        tx.send(Incoming::Notification {
            method: "_claude/sdkMessage".into(),
            params: json!({ "sessionId": "s", "message": { "type": "result", "is_error": false, "total_cost_usd": 0.01 } }),
        })
        .unwrap();
        let mut t = Translator::new(hub.clone(), s.id.clone(), AgentKind::ClaudeCode);
        t.raw = Some(super::super::claude::Parser::new(hub, s.id.clone()));
        let answer = std::future::ready(Ok(json!({ "stopReason": "end_turn" })));
        let r = prompt_turn(
            &conn,
            &mut incoming,
            &mut t,
            &CancellationToken::new(),
            answer,
            None,
            "s",
        )
        .await
        .unwrap();
        assert!(r.is_some_and(|r| r.is_ok()));
        assert!(t.raw.as_ref().unwrap().finished());
        let m = store.messages(&s.id).unwrap();
        assert!(m.iter().any(|m| m.kind == "result"));
    }

    /// Claude Code's chat comes from its own messages, which tab's Claude
    /// Code reader turns into the chat as it always has; the ACP updates
    /// that say the same are skipped.
    #[tokio::test]
    async fn claude_codes_own_messages_are_the_chat() {
        let (hub, store, s) = hub_and_session("tab-acp-test-claude");
        // The message that started the turn, as `start_turn` stores it.
        hub.post(
            &s.id,
            "user",
            "text",
            "list files",
            json!({ "agent_uuid": "tabs-guess" }),
        );
        let (client_io, _agent_io) = tokio::io::duplex(1 << 10);
        let (cr, cw) = tokio::io::split(client_io);
        let (conn, _incoming) = Conn::start(cw, cr);
        let mut parser = super::super::claude::Parser::new(hub.clone(), s.id.clone());
        parser.learn_prompt_uuid = true;
        let mut t = Translator::new(hub.clone(), s.id.clone(), AgentKind::ClaudeCode);
        t.raw = Some(parser);
        let raw = |m: Value| {
            Some(Incoming::Notification {
                method: "_claude/sdkMessage".into(),
                params: json!({ "sessionId": "s", "message": m }),
            })
        };
        let lines = [
            json!({ "type": "system", "subtype": "init", "session_id": "conv-1", "mcp_servers": [], "slash_commands": [] }),
            // The echo of the person's message, with the id Claude Code gave it.
            json!({ "type": "user", "uuid": "real-uuid", "message": { "role": "user", "content": [{ "type": "text", "text": "list files" }] } }),
            json!({ "type": "assistant", "uuid": "a1", "message": { "id": "m1", "content": [{ "type": "text", "text": "One file." }] } }),
            json!({ "type": "system", "subtype": "background_tasks_changed", "tasks": [{ "task_id": "b1", "task_type": "local_bash", "description": "watch" }] }),
        ];
        for l in lines {
            assert!(handle(&conn, &mut t, raw(l)).unwrap());
        }
        // The same text as an ACP update isn't posted twice.
        update(
            &mut t,
            json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "One file." } }),
        );
        assert!(t.raw.as_ref().unwrap().background.contains("b1"));
        let m = store.messages(&s.id).unwrap();
        let texts: Vec<_> = m
            .iter()
            .filter(|m| m.kind == "text")
            .map(|m| m.content.as_str())
            .collect();
        assert_eq!(texts, ["list files", "One file."]);
        // A rewind to that message names Claude Code's id for it.
        assert_eq!(m[0].meta["agent_uuid"], "real-uuid");
        assert_eq!(
            store
                .session_get(&s.id)
                .unwrap()
                .unwrap()
                .agent_session_id
                .as_deref(),
            Some("conv-1")
        );
    }

    #[tokio::test]
    async fn claude_code_never_switches_to_bypass_permissions() {
        let (hub, _store, s) = hub_and_session("tab-acp-test-modes");
        let (client_io, agent_io) = tokio::io::duplex(1 << 14);
        let (cr, cw) = tokio::io::split(client_io);
        let (conn, mut incoming) = Conn::start(cw, cr);
        let (ar, mut aw) = tokio::io::split(agent_io);
        let (tx, mut rx) = mpsc::unbounded_channel();
        tx.send(Steer::Mode("bypassPermissions".into())).unwrap();
        tx.send(Steer::Mode("plan".into())).unwrap();
        let agent = tokio::spawn(async move {
            let mut lines = BufReader::new(ar).lines();
            let prompt: Value =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            let mode: Value =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(mode["method"], "session/set_mode");
            assert_eq!(mode["params"]["modeId"], "plan");
            for id in [&mode["id"], &prompt["id"]] {
                let r =
                    json!({ "jsonrpc": "2.0", "id": id, "result": { "stopReason": "end_turn" } });
                aw.write_all(format!("{r}\n").as_bytes()).await.unwrap();
            }
        });
        let mut t = Translator::new(hub, s.id.clone(), AgentKind::ClaudeCode);
        prompt_turn(
            &conn,
            &mut incoming,
            &mut t,
            &CancellationToken::new(),
            conn.request(
                "session/prompt",
                acp::PromptRequest::new("s", vec![text_block("hi")]),
            ),
            Some((&mut rx, true)),
            "s",
        )
        .await
        .unwrap();
        agent.await.unwrap();
    }

    #[test]
    fn ask_user_question_goes_through_tabs_card_and_back() {
        // As claude-agent-acp sends it (src/elicitation.ts).
        let params = json!({
            "mode": "form", "sessionId": "s", "message": "Please answer the following questions.",
            "requestedSchema": { "type": "object", "properties": {
                "question_0": { "type": "string", "title": "DB", "description": "Which database?",
                    "oneOf": [{ "const": "Postgres", "title": "Postgres" }, { "const": "SQLite", "title": "SQLite" }] },
                "question_0_custom": { "type": "string", "title": "Other" },
                "question_1": { "type": "array", "title": "Extras", "description": "Which extras?",
                    "items": { "anyOf": [{ "const": "Auth", "title": "Auth" }, { "const": "Search", "title": "Search" }] } },
                "question_1_custom": { "type": "string", "title": "Other" },
            } }
        });
        let input = ask_user_question(&params).unwrap();
        assert_eq!(input["questions"][0]["question"], "Which database?");
        assert_eq!(input["questions"][0]["options"][1]["label"], "SQLite");
        assert_eq!(input["questions"][1]["multiSelect"], true);
        let d = Decision::Answer(
            json!({ "Which database?": "DuckDB", "Which extras?": "Auth, Billing" }),
        );
        let a = form_answer(&input, &d);
        assert_eq!(a["action"], "accept");
        assert_eq!(a["content"]["question_0_custom"], "DuckDB");
        assert_eq!(a["content"]["question_1"], json!(["Auth"]));
        assert_eq!(a["content"]["question_1_custom"], "Billing");
        assert_eq!(
            form_answer(&input, &Decision::Deny("no".into()))["action"],
            "decline"
        );
        assert!(
            ask_user_question(&json!({ "requestedSchema": { "properties": { "email": {} } } }))
                .is_none()
        );
    }

    #[test]
    fn permissions_pick_the_matching_option() {
        let options: Vec<acp::PermissionOption> = serde_json::from_value(json!([
            { "optionId": "once", "name": "Allow", "kind": "allow_once" },
            { "optionId": "always", "name": "Always", "kind": "allow_always" },
            { "optionId": "no", "name": "Reject", "kind": "reject_once" },
        ]))
        .unwrap();
        let pick = |d| permission_answer(&options, &d)["outcome"].clone();
        assert_eq!(pick(Decision::Allow)["optionId"], "once");
        assert_eq!(pick(Decision::AllowAlways)["optionId"], "always");
        assert_eq!(pick(Decision::Deny("no".into()))["optionId"], "no");
        let only_allow: Vec<acp::PermissionOption> = serde_json::from_value(
            json!([{ "optionId": "once", "name": "Allow", "kind": "allow_once" }]),
        )
        .unwrap();
        assert_eq!(
            permission_answer(&only_allow, &Decision::Deny("no".into()))["outcome"]["outcome"],
            "cancelled"
        );
    }

    /// A fake agent on the other end of a pipe: the client's requests and
    /// the agent's answers go through `Conn` as they would over stdio.
    #[tokio::test]
    async fn conn_matches_answers_and_passes_requests_on() {
        let (client_io, agent_io) = tokio::io::duplex(1 << 16);
        let (cr, cw) = tokio::io::split(client_io);
        let (conn, mut incoming) = Conn::start(cw, cr);
        let (ar, mut aw) = tokio::io::split(agent_io);
        let agent = tokio::spawn(async move {
            let mut lines = BufReader::new(ar).lines();
            let req: Value =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(req["method"], "initialize");
            assert_eq!(req["params"]["protocolVersion"], 1);
            let out = [
                json!({ "jsonrpc": "2.0", "id": 7, "method": "session/request_permission", "params": {} }),
                json!({ "jsonrpc": "2.0", "id": req["id"], "result": { "protocolVersion": 1 } }),
            ];
            for m in out {
                aw.write_all(format!("{m}\n").as_bytes()).await.unwrap();
            }
            let answer: Value =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(answer["id"], 7);
            assert_eq!(answer["error"]["code"], -32601);
        });
        let r = conn
            .request(
                "initialize",
                acp::InitializeRequest::new(ProtocolVersion::V1),
            )
            .await
            .unwrap();
        let init: acp::InitializeResponse = serde_json::from_value(r).unwrap();
        assert_eq!(init.protocol_version, ProtocolVersion::V1);
        match incoming.recv().await.unwrap() {
            Incoming::Request { id, method, .. } => {
                assert_eq!(method, "session/request_permission");
                conn.respond(
                    id,
                    Err(RpcError {
                        code: -32601,
                        message: "no".into(),
                    }),
                );
            }
            Incoming::Notification { .. } => panic!("expected a request"),
        }
        agent.await.unwrap();
    }
}
