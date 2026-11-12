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

/// Whether `kind`'s chats run through ACP: the person turned it on for that
/// harness and `harnesses.json` says how.
pub(crate) fn enabled(cfg: &crate::config::AgentConfig, kind: AgentKind) -> bool {
    cfg.acp.iter().any(|k| k == kind.as_str()) && catalog::harness(kind).acp.is_some()
}

pub(super) async fn run_turn(
    hub: &Arc<AgentHub>,
    session: &Session,
    prompt: &str,
    system: &str,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    // ACP has no input mid-turn: what's sent from here on waits for the next turn.
    hub.steering_close(&session.id);
    let mut queued = hub.steering_open(&session.id);
    let result = run(hub, session, prompt, system, cancel).await;
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
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    let kind = session.agent;
    let harness = catalog::harness(kind);
    let spec = harness
        .acp
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("{} has no ACP entry", harness.name))?;
    let cfg = hub.config_for(session);
    let policy = hub.policy(session);
    let strict = cfg.strict || policy.trust != Trust::Person;
    let exe = match &spec.command {
        Some(c) => super::resolve(c).map_err(|e| {
            anyhow::anyhow!(
                "{}'s ACP adapter ({c}) isn't installed: {e}{}",
                harness.name,
                spec.adapter
                    .as_ref()
                    .and_then(|a| a.npm.as_deref())
                    .map(|p| format!(". Install it with `npm install -g {p}`."))
                    .unwrap_or_default()
            )
        })?,
        None => super::agent_exe(&cfg, kind)?,
    };
    let cwd = std::path::Path::new(&session.cwd);
    let mut prepared = crate::sandbox::current()
        .command(&exe, &spec.args_for(strict), cwd, &policy)
        .await?;
    hub.add_shims(session, &mut prepared.cmd);
    for (k, v) in spec.env_for(strict) {
        prepared.cmd.env(k, v);
    }
    // Servers the person set up that strict mode turned off; a tool from
    // one of them stops the turn.
    let mut foreign = Vec::new();
    if strict && kind == AgentKind::OpenCode {
        foreign = super::opencode::own_mcp_servers(cwd, policy.trust);
        let off: serde_json::Map<_, _> = foreign
            .iter()
            .map(|n| (n.clone(), json!({ "enabled": false })))
            .collect();
        prepared
            .cmd
            .env("OPENCODE_CONFIG_CONTENT", json!({ "mcp": off }).to_string());
    }
    prepared
        .cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    tracing::debug!(session = %session.id, harness = harness.name, resume = ?session.agent_session_id, sandboxed = prepared.enforced, "starting ACP turn");
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
    events.strict = strict;
    events.mcp_tool_name = spec.mcp_tool_name.clone();
    events.foreign = foreign;
    events.allowed = vec!["tab".to_string()];

    let outcome: anyhow::Result<Option<Result<Value, RpcError>>> = async {
        // Start: initialize, then the chat's session. Updates that replay an
        // old session's history while it loads aren't new.
        events.replaying = true;
        let init = drive(
            &conn,
            &mut incoming,
            &mut events,
            &cancel,
            conn.request(
                "initialize",
                acp::InitializeRequest::new(ProtocolVersion::V1)
                    .client_capabilities(acp::ClientCapabilities::default())
                    .client_info(acp::Implementation::new("tab", env!("CARGO_PKG_VERSION"))),
            ),
        )
        .await?;
        let Some(init) = init else { return Ok(None) };
        let init: acp::InitializeResponse = serde_json::from_value(init?)?;
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

        let resume = session
            .agent_session_id
            .clone()
            .filter(|_| caps.load_session);
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
                    conn.request("session/load", load),
                )
                .await?
                else {
                    return Ok(None);
                };
                let mut r = r?;
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
                    conn.request("session/new", new),
                )
                .await?
                else {
                    return Ok(None);
                };
                let r = r?;
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
        hub.update(&session.id, None, Some(&acp_session));
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
        let r = drive(
            &conn,
            &mut incoming,
            &mut events,
            &cancel,
            conn.request("session/prompt", request),
        )
        .await?;
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

    let finish = match outcome {
        Ok(Some(Ok(r))) => {
            let r: acp::PromptResponse = serde_json::from_value(r)?;
            events.finish(Ok(r.stop_reason));
            Ok(())
        }
        Ok(Some(Err(e))) => {
            events.finish(Err(e.to_string()));
            Ok(())
        }
        Ok(None) => Ok(()),
        Err(e) => Err(e),
    };
    child.kill_tree();
    let _ = child.wait().await;
    let stderr = err_task.await.unwrap_or_default();
    if let Some(what) = events.leaked.take() {
        anyhow::bail!(
            "Strict mode stopped this turn: {} used {what}, which isn't one of tab's connections.",
            harness.name
        );
    }
    finish.map_err(|e| {
        anyhow::anyhow!(
            "{} stopped before finishing: {e}{}",
            harness.name,
            super::per_turn::tail(&stderr)
        )
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
    if let Some(o) = option {
        let offered = o["options"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|o| match o["options"].as_array() {
                Some(group) => group.clone(),
                None => vec![o.clone()],
            })
            .any(|o| o["value"] == model);
        if offered {
            let _ = conn
                .request_value(
                    "session/set_config_option",
                    json!({ "sessionId": session, "configId": o["id"], "value": model }),
                )
                .await;
            return;
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
        return;
    }
    tracing::debug!(model, "the ACP agent doesn't offer this model");
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
    let deadline = tokio::time::sleep(START_TIMEOUT);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            r = &mut fut => return Ok(Some(r)),
            m = incoming.recv() => match m {
                Some(Incoming::Notification { method, params }) => {
                    if method == "session/update" {
                        match serde_json::from_value::<acp::SessionNotification>(params) {
                            Ok(n) => events.update(&n.update),
                            Err(e) => tracing::debug!(error = %e, "unreadable session/update"),
                        }
                    }
                    if events.leaked.is_some() {
                        anyhow::bail!("strict");
                    }
                }
                Some(Incoming::Request { id, method, params }) => {
                    on_request(conn, events, id, &method, params);
                }
                None => anyhow::bail!("it closed the connection"),
            },
            _ = cancel.cancelled() => return Ok(None),
            // Only the start has a deadline; a turn runs as long as it needs.
            _ = &mut deadline, if events.replaying => anyhow::bail!("it didn't answer in time"),
        }
    }
}

/// A request from the agent. Permissions go to the person; tab offers no
/// file system or terminal (the agent uses its own, under the sandbox).
fn on_request(conn: &Conn, events: &Translator, id: Value, method: &str, params: Value) {
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
    cost: f64,
    /// Updates that replay a loaded session's history.
    replaying: bool,
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
}

impl Translator {
    fn new(hub: Arc<AgentHub>, session_id: String, kind: AgentKind) -> Self {
        Self {
            hub,
            session_id,
            kind,
            text: String::new(),
            tools: HashMap::new(),
            cost: 0.0,
            replaying: false,
            finished: false,
            strict: false,
            mcp_tool_name: None,
            allowed: Vec::new(),
            foreign: Vec::new(),
            leaked: None,
        }
    }

    fn update(&mut self, u: &acp::SessionUpdate) {
        if let acp::SessionUpdate::AvailableCommandsUpdate(c) = u {
            self.commands(&c.available_commands);
            return;
        }
        if self.replaying {
            return;
        }
        let sid = self.session_id.clone();
        match u {
            acp::SessionUpdate::AgentMessageChunk(c) => {
                if let acp::ContentBlock::Text(t) = &c.content {
                    self.hub.delta(&sid, &t.text);
                    self.text.push_str(&t.text);
                }
            }
            acp::SessionUpdate::ToolCall(tc) => {
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
            acp::SessionUpdate::UsageUpdate(u) => {
                if let Some(c) = u.cost.as_ref().filter(|c| c.currency == "USD") {
                    // The session's total so far.
                    self.cost = c.amount;
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
                kind: SlashKind::Command,
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

    fn finish(&mut self, stop: Result<acp::StopReason, String>) {
        if std::mem::replace(&mut self.finished, true) {
            return;
        }
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
            json!({ "cost_usd": self.cost }),
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
/// names the server outright; `{server}_{tool}` can only be matched against
/// the person's own servers.
fn foreign_server(
    name: &str,
    format: Option<&str>,
    allowed: &[String],
    foreign: &[String],
) -> Option<String> {
    if let Some(rest) = name.strip_prefix("mcp__") {
        let server = rest.split("__").next().unwrap_or(rest);
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
        assert_eq!(
            foreign_server("github_create_issue", None, &allowed, &foreign),
            None
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
