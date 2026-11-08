//! tab's local MCP server (Streamable HTTP, JSON responses).
//!
//! Agents started by tab get it automatically; any other MCP client can use
//! it with the URL and token from Settings. Listens on 127.0.0.1 only and
//! requires the bearer token on every request.
//!
//! Tools:
//! * `tab_search`, `tab_get_item`: read the queue.
//! * `tab_add_found`: file something relevant under the item's Found section.
//! * `tab_attach_context`: attach a queue item (or link) as Context.
//! * `tab_create_task`: create a tab task, optionally scheduled.
//! * `tab_graph_search`, `tab_graph_expand`: the knowledge graph (repos,
//!   people, tickets, projects) and the items around each node.
//! * `tab_project`, `tab_project_note`, `tab_project_update`,
//!   `tab_project_repo`: a project's overview (what it is, its rules, repos,
//!   notes and recent work), and what agents keep current for the next chat.
//! * `tab_list_connections`, `tab_find_email_server`, `tab_configure_connection`,
//!   `tab_ask_for_secret`, `tab_test_connection`: the setup assistant. Agents
//!   fill in everything but secrets; secrets go through a card in the chat
//!   that the person types into, so the agent never sees them.
//! * `tab_list_actions`, `tab_list_workflows`, `tab_save_workflow`,
//!   `tab_test_workflow`: build workflows. Agents draft and dry-run them;
//!   only the person turns one on.
//! * `approve`: Claude Code's `--permission-prompt-tool`; asks the person.

use std::sync::Arc;

use async_trait::async_trait;
use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use chrono::Utc;
use serde_json::{Value, json};

use crate::agents::{AgentHub, Decision, summarize_tool};
use crate::config::ConnectorConfig;
use crate::engine::EngineEvent;
use crate::model::Session;
use crate::model::{
    ConnectorInfo, ContextSection, Entity, ItemState, NewContext, NewTask, Preview, Project,
    ProjectNote, ProjectRepo, QueueItem, SettingKind,
};
use crate::sandbox::broker;
use crate::store::{AuditEntry, Store};
use crate::workflow::{self, Run, SavedBy, Workflow};
use tab_domain::CliEffect;

/// What the MCP server needs from the running engine.
#[async_trait]
pub trait EngineAccess: Send + Sync + 'static {
    async fn create_task(&self, task: NewTask) -> anyhow::Result<QueueItem>;
    async fn preview(&self, item_id: &str) -> anyhow::Result<Preview>;

    /// Connector types and their live state.
    async fn connectors(&self) -> anyhow::Result<Vec<ConnectorInfo>> {
        anyhow::bail!("connections can't be changed from here")
    }
    /// One connection's saved settings, secrets included. Never sent to agents.
    async fn connector_config(&self, _id: &str) -> anyhow::Result<Option<ConnectorConfig>> {
        anyhow::bail!("connections can't be changed from here")
    }
    /// Saves one connection and restarts the engine with it.
    async fn save_connector(&self, _id: &str, _cfg: ConnectorConfig) -> anyhow::Result<()> {
        anyhow::bail!("connections can't be changed from here")
    }
    /// The token a connection's CLI runs with: its own setting, or for
    /// GitHub the GitHub CLI's sign-in. Never sent to agents.
    async fn cli_token(&self, connector: &str) -> Option<String> {
        let cfg = self.connector_config(connector).await.ok().flatten();
        if let Some(t) = cfg.as_ref().and_then(|c| c.str("token")) {
            return Some(t.to_string());
        }
        if connector == "github" {
            return tokio::task::spawn_blocking(crate::gh::token)
                .await
                .ok()
                .flatten();
        }
        None
    }
    /// The things behind `pages` changed (an agent merged a pull request):
    /// drop what tab cached about them and refresh what shows them. See
    /// [`crate::engine::Engine::pages_changed`].
    async fn pages_changed(&self, connector: &str, pages: &[String]) {
        crate::connectors::forget_pages(connector, pages);
    }
    /// Dry-runs a workflow, on an item if given. Nothing changes.
    async fn dry_run_workflow(
        &self,
        _w: Workflow,
        _item_id: Option<String>,
    ) -> anyhow::Result<Run> {
        anyhow::bail!("workflows can't be tested from here")
    }
}

pub struct McpServer {
    pub store: Arc<Store>,
    pub hub: Arc<AgentHub>,
    pub engine: Arc<dyn EngineAccess>,
    pub events: tokio::sync::broadcast::Sender<EngineEvent>,
    pub token: String,
}

/// Starts the server on the first free port from `port`. Returns the URL.
pub async fn serve(server: Arc<McpServer>, port: u16) -> anyhow::Result<String> {
    let mut listener = None;
    for p in port..port.saturating_add(20) {
        if let Ok(l) = tokio::net::TcpListener::bind(("127.0.0.1", p)).await {
            listener = Some(l);
            break;
        }
    }
    let listener = listener.ok_or_else(|| anyhow::anyhow!("no free port for the MCP server"))?;
    let url = format!("http://{}/mcp", listener.local_addr()?);
    let app = Router::new()
        .route(
            "/mcp",
            post(handle).get(|| async { StatusCode::METHOD_NOT_ALLOWED }),
        )
        .route("/cli", post(handle_cli))
        .route(
            "/git/{session}/{connector}/{*rest}",
            axum::routing::any(handle_git),
        )
        .with_state(server);
    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::error!(error = %e, "MCP server stopped");
        }
    });
    Ok(url)
}

/// The token one chat uses with tab: it names the chat, so a chat can't
/// act as another (a review chat can't borrow the person's own chat's
/// trust). Derived from tab's own token, which only tab and the person see.
pub fn chat_token(master: &str, session: &str) -> String {
    use sha2::{Digest, Sha256};
    let d = Sha256::digest(format!("tab-chat\n{master}\n{session}"));
    d.iter().map(|b| format!("{b:02x}")).collect()
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
}

/// tab's own token: the person's MCP clients, from Settings.
fn authed(s: &McpServer, headers: &HeaderMap) -> bool {
    bearer(headers).is_some_and(|t| !s.token.is_empty() && crate::hub_tls::same_secret(t, &s.token))
}

/// This chat's own token.
fn authed_chat(s: &McpServer, headers: &HeaderMap, session: &str) -> bool {
    !s.token.is_empty()
        && !session.is_empty()
        && bearer(headers)
            .is_some_and(|t| crate::hub_tls::same_secret(t, &chat_token(&s.token, session)))
}

/// `tab __tab_cli`: a chat's `gh` or `git` command, run by tab with the
/// connection's token (see [`crate::sandbox::broker`]).
async fn handle_cli(State(s): State<Arc<McpServer>>, headers: HeaderMap, body: String) -> Response {
    let id = headers
        .get("x-tab-session")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    if !authed_chat(&s, &headers, &id) {
        return (StatusCode::UNAUTHORIZED, "missing or wrong bearer token").into_response();
    }
    let session = s.store.session_get(&id).ok().flatten();
    let Some(session) = session else {
        return (
            StatusCode::FORBIDDEN,
            "only tab's chats can run commands through tab",
        )
            .into_response();
    };
    let Ok(req) = serde_json::from_str::<broker::Request>(&body) else {
        return (StatusCode::BAD_REQUEST, "unreadable command").into_response();
    };
    let reply = s.run_cli(&session, req).await;
    axum::Json(reply).into_response()
}

fn upstream_client() -> &'static reqwest::Client {
    static C: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    C.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .user_agent("git/2 (tab)")
            .build()
            .expect("reqwest client")
    })
}

/// A chat's git traffic to a host a connection signs in (see
/// `broker::git_config`). tab forwards it with the connection's token, so
/// git in the chat never holds it, and checks a push's ref updates first.
async fn handle_git(
    State(s): State<Arc<McpServer>>,
    axum::extract::Path((session, connector, rest)): axum::extract::Path<(String, String, String)>,
    axum::extract::RawQuery(query): axum::extract::RawQuery,
    method: axum::http::Method,
    headers: HeaderMap,
    body: axum::body::Body,
) -> Response {
    if !authed_chat(&s, &headers, &session) {
        return (StatusCode::UNAUTHORIZED, "missing or wrong bearer token").into_response();
    }
    let Some(session) = s.store.session_get(&session).ok().flatten() else {
        return (StatusCode::FORBIDDEN, "only tab's chats can use this").into_response();
    };
    let Some((tool, base)) = broker::all().into_iter().find_map(|b| match &b.cli.auth {
        tab_domain::CliAuth::GitEndpoint { url, .. } if b.connector == connector => {
            let url = url.clone();
            Some((b, url))
        }
        _ => None,
    }) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    // `owner/repo(.git)/info/refs`, `…/git-upload-pack`, `…/git-receive-pack`.
    let service = ["info/refs", "git-upload-pack", "git-receive-pack"]
        .into_iter()
        .find(|e| rest.ends_with(e));
    let Some(service) = service else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let repo = rest
        .trim_end_matches(service)
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .to_string();
    if repo.split('/').filter(|p| !p.is_empty()).count() != 2 || repo.contains("..") {
        return StatusCode::NOT_FOUND.into_response();
    }
    #[cfg(test)]
    let base = tests::UPSTREAM.lock().unwrap().clone().unwrap_or(base);
    let mut url = format!("{}/{rest}", base.trim_end_matches('/'));
    if let Some(q) = query {
        url = format!("{url}?{q}");
    }
    let token = s.engine.cli_token(&tool.connector).await;
    let started = std::time::Instant::now();

    // Pushes: read the ref updates, and ask the person when that's due.
    let mut stream = body.into_data_stream();
    let mut prefix: Vec<u8> = Vec::new();
    let mut pushed: Vec<broker::RefUpdate> = Vec::new();
    let mut effect = if service == "git-receive-pack" {
        CliEffect::Write
    } else {
        CliEffect::Read
    };
    if service == "git-receive-pack" && method == axum::http::Method::POST {
        use futures_util::StreamExt;
        let parsed = loop {
            if let Some(p) = broker::parse_push(&prefix) {
                break Some(p);
            }
            match stream.next().await {
                Some(Ok(chunk)) => prefix.extend_from_slice(&chunk),
                _ => break None,
            }
            if prefix.len() > 1 << 20 {
                break None;
            }
        };
        let Some((updates, caps, _)) = parsed else {
            return (StatusCode::BAD_REQUEST, "unreadable push").into_response();
        };
        let mut what = Vec::new();
        for u in &updates {
            let (e, w) = if u.deletes() {
                (CliEffect::Destructive, format!("delete {}", u.short()))
            } else if u.creates() {
                (CliEffect::Write, format!("create {}", u.short()))
            } else if fast_forward(&session.cwd, &u.old, &u.new).await {
                (CliEffect::Write, format!("update {}", u.short()))
            } else if own_branch(&s.store, &session.id, &repo, u) {
                (
                    CliEffect::Write,
                    format!("force-update {} (its own branch)", u.short()),
                )
            } else {
                (
                    CliEffect::Destructive,
                    format!("force-update {} (rewrites its history)", u.short()),
                )
            };
            if e == CliEffect::Destructive {
                effect = CliEffect::Destructive;
            }
            what.push(w);
        }
        let policy = s.hub.policy(&session);
        let ask = effect == CliEffect::Destructive
            || (effect == CliEffect::Write && policy.trust != crate::sandbox::Trust::Person);
        if ask {
            let summary = format!("Push to {repo}: {}", what.join(", "));
            let decision = s
                .hub
                .request_approval(
                    &session.id,
                    "git",
                    summary,
                    json!({ "repo": repo, "updates": what, "connection": tool.connector }),
                )
                .await;
            if let Decision::Deny(why) = decision {
                // Let git finish sending, then turn it down the way it understands.
                use futures_util::StreamExt;
                while stream.next().await.is_some() {}
                let why = format!("the person didn't allow this push: {why}");
                s.log_cli(
                    &tool.connector,
                    &session.id,
                    effect,
                    &format!("git push {repo} ({})", what.join(", ")),
                    1,
                    started,
                    Some(why.clone()),
                );
                return (
                    [(
                        axum::http::header::CONTENT_TYPE,
                        "application/x-git-receive-pack-result",
                    )],
                    broker::push_refused(&updates, &caps, &why),
                )
                    .into_response();
            }
        }
        pushed = updates;
    }

    let mut req = upstream_client().request(method.clone(), &url);
    for h in [
        "content-type",
        "accept",
        "content-encoding",
        "git-protocol",
        "accept-encoding",
    ] {
        if let Some(v) = headers.get(h) {
            req = req.header(h, v);
        }
    }
    if let Some(t) = &token {
        use base64::Engine as _;
        req = req.header(
            "authorization",
            format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode(format!("x-access-token:{t}"))
            ),
        );
    }
    if method == axum::http::Method::POST {
        use futures_util::StreamExt;
        let head = futures_util::stream::once(async move {
            Ok::<_, axum::Error>(axum::body::Bytes::from(prefix))
        });
        req = req.body(reqwest::Body::wrap_stream(head.chain(stream)));
    }
    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                format!("reaching GitHub failed: {e}"),
            )
                .into_response();
        }
    };
    let status = resp.status();
    if service != "info/refs" {
        let label = if pushed.is_empty() {
            format!("git fetch {repo}")
        } else {
            format!(
                "git push {repo} ({})",
                pushed
                    .iter()
                    .map(|u| u.short().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        let ok = status.is_success();
        s.log_cli(
            &tool.connector,
            &session.id,
            effect,
            &label,
            if ok { 0 } else { 1 },
            started,
            (!ok).then(|| status.to_string()),
        );
        if ok {
            remember_branches(&s.store, &session.id, &repo, &pushed);
        }
        if ok && !pushed.is_empty() {
            let change = tab_domain::CliChange {
                connector: tool.connector.clone(),
                command: label,
                effect,
                resource: "branch".into(),
                repo: Some(repo.clone()),
                number: None,
                url: tool.cli.page("branch", Some(&repo), None),
            };
            s.engine
                .pages_changed(&change.connector, change.url.as_slice())
                .await;
            let _ = s.events.send(EngineEvent::CliChanged {
                session_id: session.id.clone(),
                change,
            });
        }
    }
    let mut out = Response::builder().status(status.as_u16());
    for h in [
        "content-type",
        "cache-control",
        "content-encoding",
        "expires",
        "pragma",
    ] {
        if let Some(v) = resp.headers().get(h) {
            out = out.header(h, v.as_bytes());
        }
    }
    out.body(axum::body::Body::from_stream(resp.bytes_stream()))
        .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response())
}

/// Whether a chat may rewrite this branch without asking: it created the
/// branch, and the branch is still at the commit the chat last pushed, so
/// nobody else's commits would be lost.
fn own_branch(store: &Store, session: &str, repo: &str, u: &broker::RefUpdate) -> bool {
    u.name.starts_with("refs/heads/")
        && matches!(
            store.session_branch_head(session, repo, &u.name),
            Ok(Some(head)) if head == u.old
        )
}

/// After a push goes through, tracks the branches the chat created and
/// what it last pushed to them.
fn remember_branches(store: &Store, session: &str, repo: &str, pushed: &[broker::RefUpdate]) {
    for u in pushed.iter().filter(|u| u.name.starts_with("refs/heads/")) {
        let r = if u.deletes() {
            store.session_branch_forget(session, repo, &u.name)
        } else if u.creates()
            || matches!(
                store.session_branch_head(session, repo, &u.name),
                Ok(Some(_))
            )
        {
            store.session_branch_set(session, repo, &u.name, &u.new)
        } else {
            Ok(())
        };
        if let Err(e) = r {
            tracing::warn!("couldn't record pushed branch {}: {e}", u.name);
        }
    }
}

/// Whether `new` keeps `old`'s history (a fast-forward). GitHub doesn't
/// have the new commits before the push, so this asks the chat's checkout,
/// which does. When it can't tell, the push counts as a force push and the
/// person is asked.
async fn fast_forward(cwd: &str, old: &str, new: &str) -> bool {
    let Ok(git) = which::which("git") else {
        return false;
    };
    let cwd = std::path::Path::new(cwd);
    let cfg = broker::git_hardening(&git, cwd).await;
    let mut cmd = crate::agents::command(&git);
    cmd.args(["merge-base", "--is-ancestor", old, new])
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .env("GIT_CONFIG_COUNT", cfg.len().to_string());
    for (i, (k, v)) in cfg.iter().enumerate() {
        cmd.env(format!("GIT_CONFIG_KEY_{i}"), k);
        cmd.env(format!("GIT_CONFIG_VALUE_{i}"), v);
    }
    matches!(
        tokio::time::timeout(std::time::Duration::from_secs(10), cmd.status()).await,
        Ok(Ok(st)) if st.success()
    )
}

async fn handle(State(s): State<Arc<McpServer>>, headers: HeaderMap, body: String) -> Response {
    let session = headers
        .get("x-tab-session")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    // tab's own token, or the token of the chat the request names.
    if !authed(&s, &headers) && !authed_chat(&s, &headers, session.as_deref().unwrap_or("")) {
        return (StatusCode::UNAUTHORIZED, "missing or wrong bearer token").into_response();
    }
    let Ok(req) = serde_json::from_str::<Value>(&body) else {
        return json_response(rpc_error(Value::Null, -32700, "parse error"));
    };
    match req {
        Value::Array(batch) => {
            let mut out = Vec::new();
            for r in batch {
                if let Some(resp) = s.dispatch(session.as_deref(), r).await {
                    out.push(resp);
                }
            }
            if out.is_empty() {
                StatusCode::ACCEPTED.into_response()
            } else {
                json_response(Value::Array(out))
            }
        }
        r => match s.dispatch(session.as_deref(), r).await {
            Some(resp) => json_response(resp),
            None => StatusCode::ACCEPTED.into_response(),
        },
    }
}

fn json_response(v: Value) -> Response {
    ([("content-type", "application/json")], v.to_string()).into_response()
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn text(t: impl Into<String>) -> Value {
    json!({ "content": [{ "type": "text", "text": t.into() }] })
}

fn brief(i: &QueueItem) -> Value {
    json!({
        "id": i.id,
        "kind": i.event.kind,
        "title": i.event.title,
        "from": i.event.actor,
        "where": i.event.context,
        "url": i.event.url,
        "when": i.event.occurred_at.format("%Y-%m-%d %H:%M UTC").to_string(),
        "state": i.state,
        "urgency": i.ranking.as_ref().map(|r| r.urgency),
        "score": i.ranking.as_ref().map(|r| r.score),
    })
}

fn entity_json(e: &Entity) -> Value {
    json!({ "entity_id": e.id, "kind": e.kind, "label": e.label, "key": e.key, "items": e.item_count })
}

fn note_json(n: &ProjectNote) -> Value {
    json!({
        "topic": n.topic, "text": n.text, "by": n.added_by,
        "updated": n.updated_at.format("%Y-%m-%d").to_string(), "sources": n.sources,
    })
}

fn repo_json(r: &ProjectRepo) -> Value {
    json!({ "repo": r.repo, "path": r.path, "note": r.note, "by": r.added_by })
}

/// The tools an agent needs in most chats. Claude Code keeps MCP tools
/// behind ToolSearch, where the model only sees their names and rarely
/// loads them; these are marked to load up front so it reaches for them.
/// The rest stay deferred, so they cost no context until needed.
const ALWAYS_LOAD: &[&str] = &[
    "tab_steps",
    "tab_search",
    "tab_get_item",
    "tab_attach_context",
    "tab_add_found",
    // Projects: agents read a project's context and keep it current.
    "tab_project",
    "tab_project_note",
    "tab_project_update",
    "tab_project_repo",
    "tab_set_project",
    "tab_graph_search",
];

/// Marks each tool for Claude Code: `anthropic/alwaysLoad` for the ones in
/// [`ALWAYS_LOAD`], false for the rest. Other agents ignore `_meta`.
fn with_load_hints(mut tools: Value) -> Value {
    for t in tools.as_array_mut().into_iter().flatten() {
        let always = t["name"].as_str().is_some_and(|n| ALWAYS_LOAD.contains(&n));
        t["_meta"] = json!({ "anthropic/alwaysLoad": always });
    }
    tools
}

pub fn tools() -> Value {
    json!([
        {
            "name": "tab_steps",
            "description": "Show the person your plan as a step list at the end of the chat, each with its status. Call it before starting work that takes more than a couple of tool calls, then again whenever a step starts or finishes; each call replaces the whole list. Keep one step in_progress at a time. List every step to the finished result up front (not just the one you're on) so the person can see how much is left. Each step is a short outcome in plain words (\"Find where Done is handled\", \"Open a PR\"), never a command, tool name or file path.",
            "inputSchema": { "type": "object", "properties": {
                "steps": { "type": "array", "items": { "type": "object", "properties": {
                    "title": { "type": "string", "description": "What the step does, e.g. \"Find where the chat scrolls\"." },
                    "status": { "type": "string", "enum": ["pending", "in_progress", "completed", "cancelled"] }
                }, "required": ["title", "status"] } }
            }, "required": ["steps"] }
        },
        {
            "name": "tab_search",
            "description": "Search the person's work (Slack, GitHub, email, meetings, Jira, tab tasks) by keyword and by meaning, so both exact names and paraphrases match. Narrow it to a time window and/or a project for more relevant results; if a window finds too little it widens automatically (say so if you rely on older results). Also returns project notes that match. Returns items with ids you can attach or read.",
            "inputSchema": { "type": "object", "properties": {
                "query": { "type": "string", "description": "Words or a short description of what you're looking for." },
                "within_days": { "type": "integer", "minimum": 1, "description": "Only things from the last N days. Start small (7 or 30) for recent work." },
                "since": { "type": "string", "description": "ISO date or time; only things at or after it." },
                "until": { "type": "string", "description": "ISO date or time; only things before it." },
                "project": { "type": "string", "description": "Project name or id, or \"current\" for this chat's project." },
                "expand": { "type": "boolean", "description": "Widen the window when it finds fewer than 3 results. Default true." },
                "include_done": { "type": "boolean" },
                "limit": { "type": "integer", "minimum": 1, "maximum": 50 }
            }, "required": ["query"] }
        },
        {
            "name": "tab_get_item",
            "description": "Read one queue item with its body, rich preview and attached context.",
            "inputSchema": { "type": "object", "properties": {
                "item_id": { "type": "string" }
            }, "required": ["item_id"] }
        },
        {
            "name": "tab_add_found",
            "description": "File something that might be relevant but isn't directly part of the work (a similar PR, a related discussion, a doc) under Found on the current task, so the person can decide whether to add it to context.",
            "inputSchema": { "type": "object", "properties": {
                "title": { "type": "string" },
                "url": { "type": "string" },
                "ref_item_id": { "type": "string", "description": "A tab item id, if what you found is in the queue." },
                "summary": { "type": "string", "description": "One or two sentences on why it matters." },
                "item_id": { "type": "string", "description": "Task to file it on. Defaults to this session's task." },
                "project": { "type": "string", "description": "Optional. Also file the found queue item under this project (name or id), or \"none\". Found items keep their project unless you pass one." }
            }, "required": ["title"] }
        },
        {
            "name": "tab_attach_context",
            "description": "Attach a queue item or link directly to the current task's Context. Use for things that are directly part of the work: the PR you opened for it, the ticket it implements, the thread or email you're answering, or anything the person names. For things that only might be relevant, use tab_add_found instead. A link to something in the queue attaches that item. The attached item joins this chat's project unless it already has one; pass `project` when it clearly belongs elsewhere (another repo or team), or \"none\" for things that aren't project work, and say which you chose.",
            "inputSchema": { "type": "object", "properties": {
                "ref_item_id": { "type": "string" },
                "url": { "type": "string" },
                "title": { "type": "string" },
                "item_id": { "type": "string", "description": "Task to attach to. Defaults to this session's task." },
                "project": { "type": "string", "description": "Optional. Project (name or id) to file the attached item under, or \"none\". Defaults to this chat's project when the item has none." }
            } }
        },
        {
            "name": "tab_create_task",
            "description": "Create a task in tab's queue, optionally scheduled for later.",
            "inputSchema": { "type": "object", "properties": {
                "title": { "type": "string" },
                "body": { "type": "string" },
                "priority": { "type": "string", "enum": ["low", "normal", "high", "urgent"] },
                "due_in_minutes": { "type": "integer", "minimum": 1 },
                "url": { "type": "string" },
                "project": { "type": "string", "description": "Project name or id. Defaults to this chat's project." }
            }, "required": ["title"] }
        },
        {
            "name": "tab_graph_search",
            "description": "Search tab's knowledge graph: the projects, repos, people, PRs, tickets, channels and email contacts that appear across the person's work. Use it when they mention something you don't recognise. Each match comes with what it's most connected to, its recent items, and for a project its notes.",
            "inputSchema": { "type": "object", "properties": {
                "query": { "type": "string", "description": "A name or part of one: \"billing\", \"dana\", \"acme/web\"." },
                "within_days": { "type": "integer", "minimum": 1, "description": "Only count activity from the last N days, so what is connected now shows first. Leave out for all time." },
                "limit": { "type": "integer", "minimum": 1, "maximum": 10 }
            }, "required": ["query"] }
        },
        {
            "name": "tab_graph_expand",
            "description": "Follow the graph one step from an entity (entity_id from tab_graph_search): what it's connected to, optionally only one kind (person, repo, pull_request, issue, project, ...), and its recent items.",
            "inputSchema": { "type": "object", "properties": {
                "entity_id": { "type": "integer" },
                "kind": { "type": "string" },
                "within_days": { "type": "integer", "minimum": 1, "description": "Only count activity from the last N days, so what is connected now shows first. Leave out for all time." },
                "limit": { "type": "integer", "minimum": 1, "maximum": 30 }
            }, "required": ["entity_id"] }
        },
        {
            "name": "tab_project",
            "description": "A project's overview: what it is, its rules, the repos its work happens in (and where they're checked out), its notes, the repos, people and tickets its items are about, and its recent items. Read it at the start of work in a project and whenever you need to know where to work or how. Defaults to this chat's project; with no project, lists them all.",
            "inputSchema": { "type": "object", "properties": {
                "project": { "type": "string", "description": "Project name or id." },
                "within_days": { "type": "integer", "minimum": 1, "description": "Only count activity from the last N days, so what is connected now shows first. Leave out for all time." }
            } }
        },
        {
            "name": "tab_project_update",
            "description": "Write what a project is about or its rules, for every future chat in it. `about`: what the project is and does, in a few sentences. `rules`: how to work in it (conventions, branches, what not to touch, who to ask), as short lines. Each replaces the whole field, so read tab_project first and keep what's still true. Record rules the person states or the repo documents; don't invent them.",
            "inputSchema": { "type": "object", "properties": {
                "about": { "type": "string" },
                "rules": { "type": "string" },
                "project": { "type": "string", "description": "Project name or id. Defaults to this chat's project." }
            } }
        },
        {
            "name": "tab_project_repo",
            "description": "Add a repo the project's work happens in, update it, or remove it, so future chats know where to work. Add one when you work in a repo for this project that isn't listed yet. Saving the same repo again updates its path or note.",
            "inputSchema": { "type": "object", "properties": {
                "repo": { "type": "string", "description": "owner/name, a GitHub URL, or another repo URL." },
                "path": { "type": "string", "description": "Where it's checked out on this machine, if you know." },
                "note": { "type": "string", "description": "What the repo is for in this project, in one line." },
                "remove": { "type": "boolean", "description": "Remove it from the project instead." },
                "project": { "type": "string", "description": "Project name or id. Defaults to this chat's project." }
            }, "required": ["repo"] }
        },
        {
            "name": "tab_set_project",
            "description": "Put a queue item in a project, or take it out (project null). Use this when the person asks to link, file or move something into a project. Defaults to this chat's item.",
            "inputSchema": { "type": "object", "properties": {
                "item_id": { "type": "string" },
                "project": { "type": ["string", "null"], "description": "Project name or id; null to remove it from its project." }
            }, "required": ["project"] }
        },
        {
            "name": "tab_project_note",
            "description": "Save a durable fact about a project for future chats: how it deploys, who owns what, where something lives, a decision and why. One note per topic: writing a topic again replaces it. Cite the items it came from. Every future chat in the project reads these, so never note one-off events (a sign-in, a verification, a receipt, a code), credentials, codes, or personal account details. To put an item in a project, use tab_set_project instead.",
            "inputSchema": { "type": "object", "properties": {
                "topic": { "type": "string", "description": "A short, stable topic, e.g. \"deploys\", \"owners\", \"staging env\"." },
                "text": { "type": "string", "description": "The fact, in one to three sentences." },
                "source_item_ids": { "type": "array", "items": { "type": "string" } },
                "project": { "type": "string", "description": "Project name or id. Defaults to this chat's project." },
                "delete": { "type": "boolean", "description": "Remove the note on this topic instead, e.g. when it's no longer true." }
            }, "required": ["topic"] }
        },
        {
            "name": "tab_list_connections",
            "description": "List tab's connections (email, GitHub, Slack, ...): whether each is installed, its status, its settings fields and their current values. Secret values are shown only as set or not set.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "tab_find_email_server",
            "description": "Work out the IMAP server for an email address: reads its MX records, recognizes known providers (MXroute, Gmail, iCloud, Fastmail, ...), checks Thunderbird's provider database, and tests each candidate live for a valid TLS certificate and an IMAP greeting.",
            "inputSchema": { "type": "object", "properties": {
                "address": { "type": "string", "description": "The person's email address." }
            }, "required": ["address"] }
        },
        {
            "name": "tab_configure_connection",
            "description": "Save a connection's settings (everything except secrets) and install it. If a required secret such as a password is missing, a card appears in the chat for the person to type it; wait for them to say it's entered, then call tab_test_connection.",
            "inputSchema": { "type": "object", "properties": {
                "connector_id": { "type": "string" },
                "settings": { "type": "object", "description": "Setting keys from tab_list_connections and their values. Leave out secrets." },
                "enable": { "type": "boolean", "description": "Install it once everything required is set. Default true." }
            }, "required": ["connector_id", "settings"] }
        },
        {
            "name": "tab_ask_for_secret",
            "description": "Show the card in the chat where the person types a connection's secrets (password, token) again, for example after a failed sign-in. Never ask for secrets in chat text.",
            "inputSchema": { "type": "object", "properties": {
                "connector_id": { "type": "string" },
                "reason": { "type": "string", "description": "One line shown on the card, e.g. what kind of password is needed." }
            }, "required": ["connector_id"] }
        },
        {
            "name": "tab_test_connection",
            "description": "Check a connection with its saved settings. For email, signs in and reports the folders and message counts; errors come back as text to act on.",
            "inputSchema": { "type": "object", "properties": {
                "connector_id": { "type": "string" }
            }, "required": ["connector_id"] }
        },
        {
            "name": "tab_list_actions",
            "description": "What workflows can start on and do: every connection's item kinds (triggers) and actions, with each action's inputs, outputs and effect (read, write, destructive), plus the template variables a step can use. Call this before drafting a workflow.",
            "inputSchema": { "type": "object", "properties": {
                "connector_id": { "type": "string", "description": "Only this connection's. Optional." }
            } }
        },
        {
            "name": "tab_list_workflows",
            "description": "The person's workflows with their full JSON definitions, whether each is on, and their last few runs.",
            "inputSchema": { "type": "object", "properties": {
                "workflow_id": { "type": "string", "description": "Just this one, with more runs." }
            } }
        },
        {
            "name": "tab_save_workflow",
            "description": "Create or update a workflow from its JSON definition. Pass id to update one. It is checked first (actions exist, required inputs are set, templates only read earlier steps, it can't loop with other workflows) and not saved if anything is wrong; fix what it says and try again. It's always saved turned off: the person reviews it in the Workflows tab and turns it on. Missing inputs get the action's defaults.\n\nShape: {\"name\", \"description\", \"trigger\": {\"type\":\"item\",\"changes\":[\"ingested\"],\"sources\":[\"github\"],\"kinds\":[\"github.review_requested\"],\"when\":[conditions]} | {\"type\":\"signal\",\"name\"} | {\"type\":\"schedule\",\"every_minutes\"} | {\"type\":\"manual\"}, \"steps\": [{\"id\":\"pr\",\"type\":\"action\",\"connector\":\"github\",\"action\":\"pr.get\",\"input\":{...},\"when\":[conditions]}, {\"id\":\"small\",\"type\":\"filter\",\"conditions\":[...]}, {\"id\":\"vals\",\"type\":\"set\",\"values\":{...}}, {\"id\":\"tell\",\"type\":\"emit\",\"signal\":\"name\",\"payload\":{...}}, {\"id\":\"test\",\"type\":\"command\",\"program\":\"cargo\",\"args\":[\"test\"],\"folder\":\"\",\"network\":[\"crates.io\",\"*.crates.io\"],\"env\":{},\"image\":\"\",\"timeout_secs\":600}], \"limits\": {\"max_runs_per_hour\":30,\"cooldown_minutes\":0}}. A condition is {\"field\":\"item.kind\",\"op\":\"equals|not_equals|contains|not_contains|starts_with|matches|in|gt|lt|exists|missing\",\"value\":...}. Changes: ingested, updated, ranked, snoozed, woke, done, reopened. Templates: {{item.title}}, {{item.metadata.repo}}, {{item.score}}, {{steps.<id>.<output>}}, {{signal.<key>}}, {{now}}.",
            "inputSchema": { "type": "object", "properties": {
                "workflow": { "type": "object", "description": "The workflow definition. Include id to update." }
            }, "required": ["workflow"] }
        },
        {
            "name": "tab_test_workflow",
            "description": "Dry-run a workflow (saved, by id, or a draft definition) on a queue item and get each step's inputs and outputs. Reads (like reading a PR) really run; writes are only shown. Use it to check templates and conditions before handing the workflow to the person.",
            "inputSchema": { "type": "object", "properties": {
                "workflow_id": { "type": "string" },
                "workflow": { "type": "object", "description": "A draft definition, instead of workflow_id." },
                "item_id": { "type": "string", "description": "The item to run it on, from tab_search. Optional for schedule and signal triggers." }
            } }
        },
        {
            "name": "approve",
            "description": "Permission prompt: asks the person in tab whether a tool call may run.",
            "inputSchema": { "type": "object", "properties": {
                "tool_name": { "type": "string" },
                "input": { "type": "object" },
                "tool_use_id": { "type": "string" }
            }, "required": ["tool_name", "input"] }
        }
    ])
}

/// Labels of required settings that have no value yet.
fn missing_required(info: &ConnectorInfo, cfg: &ConnectorConfig) -> Vec<String> {
    info.descriptor
        .settings
        .iter()
        .filter(|f| f.required)
        .filter(|f| match cfg.settings.get(&f.key) {
            None | Some(Value::Null) => true,
            Some(Value::String(s)) => s.trim().is_empty(),
            _ => false,
        })
        .map(|f| f.label.clone())
        .collect()
}

impl McpServer {
    /// Connector types with their actions. Falls back to the registry, not
    /// installed, when the engine can't say.
    async fn catalog(&self) -> Vec<ConnectorInfo> {
        match self.engine.connectors().await {
            Ok(c) => c,
            Err(_) => crate::connectors::registry()
                .into_iter()
                .map(|f| ConnectorInfo {
                    descriptor: f.descriptor(),
                    enabled: false,
                    status: crate::model::ConnectorStatus::Disabled,
                })
                .collect(),
        }
    }

    async fn connector_info(&self, id: &str) -> anyhow::Result<ConnectorInfo> {
        self.engine
            .connectors()
            .await?
            .into_iter()
            .find(|c| c.descriptor.id == id)
            .ok_or_else(|| anyhow::anyhow!("no connection {id}; see tab_list_connections"))
    }

    /// Puts the card in the chat where the person types a connection's secrets.
    fn ask_secret(&self, session: Option<&str>, info: &ConnectorInfo, reason: Option<String>) {
        let Some(session) = session.filter(|s| !s.is_empty()) else {
            return;
        };
        let d = &info.descriptor;
        // One card per ask: if one for this connection is already waiting
        // (nothing from the person since), don't stack a second.
        if let Ok(msgs) = self.store.messages(session) {
            let waiting = msgs
                .iter()
                .rev()
                .take_while(|m| m.role != "user")
                .any(|m| m.kind == "secret_request" && m.meta["connector_id"] == d.id.as_str());
            if waiting {
                return;
            }
        }
        let fields: Vec<_> = d
            .settings
            .iter()
            .filter(|f| f.kind == SettingKind::Secret)
            .map(|f| json!({ "key": f.key, "label": f.label, "help": f.help }))
            .collect();
        self.hub.post(
            session,
            "system",
            "secret_request",
            &format!("Enter your {} details", d.name),
            json!({ "connector_id": d.id, "name": d.name, "icon": d.icon, "fields": fields, "reason": reason }),
        );
    }

    async fn dispatch(&self, session: Option<&str>, req: Value) -> Option<Value> {
        let id = req.get("id").cloned();
        let method = req["method"].as_str().unwrap_or_default();
        let id = id?; // notifications get no response
        let params = req["params"].clone();
        let result = match method {
            "initialize" => Ok(json!({
                "protocolVersion": params["protocolVersion"].as_str().unwrap_or("2025-06-18"),
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "tab", "version": env!("CARGO_PKG_VERSION") },
                "instructions": "tab is the person's work queue. Search it, read items, file what you find under Found, attach context and create tasks, and set up connections (email, GitHub, ...) for the person. When the person mentions something you don't recognise, call tab_graph_search before asking them. Leave durable facts about a project with tab_project_note."
            })),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": with_load_hints(tools()) })),
            "tools/call" => {
                let name = params["name"].as_str().unwrap_or_default();
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                Ok(match self.call(session, name, args).await {
                    Ok(v) => v,
                    Err(e) => {
                        json!({ "content": [{ "type": "text", "text": e.to_string() }], "isError": true })
                    }
                })
            }
            _ => Err((-32601, format!("unknown method {method}"))),
        };
        Some(match result {
            Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
            Err((code, m)) => rpc_error(id, code, &m),
        })
    }

    fn session_item(&self, session: Option<&str>, args: &Value) -> anyhow::Result<String> {
        if let Some(i) = args["item_id"].as_str().filter(|s| !s.is_empty()) {
            return Ok(i.to_string());
        }
        session
            .and_then(|s| self.store.session_get(s).ok().flatten())
            .and_then(|s| s.item_id)
            .ok_or_else(|| anyhow::anyhow!("pass item_id: this session is not tied to a task"))
    }

    /// The project named in `project`, else the session's task's project.
    fn project_arg(&self, session: Option<&str>, args: &Value) -> anyhow::Result<Option<Project>> {
        if let Some(name) = args["project"].as_str().filter(|v| !v.trim().is_empty()) {
            return Ok(Some(
                self.store
                    .project_find(name)?
                    .ok_or_else(|| anyhow::anyhow!("no project named {name}"))?,
            ));
        }
        let pid = self
            .session_item(session, &json!({}))
            .ok()
            .and_then(|id| self.store.get(&id).ok().flatten())
            .and_then(|i| i.project_id);
        Ok(match pid {
            Some(p) => self.store.project_find(&p)?,
            None => None,
        })
    }

    /// Writes a brokered command to the audit log, next to the connection's
    /// own API calls.
    #[allow(clippy::too_many_arguments)]
    fn log_cli(
        &self,
        connector: &str,
        session: &str,
        effect: CliEffect,
        command: &str,
        code: i32,
        started: std::time::Instant,
        error: Option<String>,
    ) {
        let effect = serde_json::to_value(effect)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default();
        let entry = AuditEntry {
            id: 0,
            ts: Utc::now(),
            provider: connector.to_string(),
            connector: connector.to_string(),
            purpose: format!("cli {effect}, chat {session}"),
            method: "CLI".into(),
            url: command.to_string(),
            outcome: if code == 0 { "network" } else { "error" }.into(),
            status: None,
            duration_ms: started.elapsed().as_millis() as u64,
            rl_resource: None,
            rl_limit: None,
            rl_remaining: None,
            rl_reset: None,
            error,
        };
        if let Err(e) = self.store.audit_insert(&entry) {
            tracing::error!(error = %e, "writing audit log failed");
        }
    }

    /// Runs a chat's brokered command: asks the person when it's
    /// destructive (or any change, in a chat about someone else's code),
    /// runs it in the chat's sandbox with the connection's token, logs it,
    /// and tells the app what changed.
    pub async fn run_cli(&self, session: &Session, req: broker::Request) -> broker::Reply {
        if req.tool == "git-credential" {
            use base64::Engine as _;
            let input = base64::engine::general_purpose::STANDARD
                .decode(req.stdin.as_deref().unwrap_or(""))
                .unwrap_or_default();
            let op = req.args.first().map(String::as_str).unwrap_or("");
            return broker::credential(op, &input).await;
        }
        let Some(tool) = broker::find(&req.tool) else {
            return broker::Reply::refused(&format!("tab doesn't run {} for chats", req.tool));
        };
        let m = tool.classify(&req.args);
        let command = broker::describe(tool.name(), &req.args);
        match m.effect {
            CliEffect::Blocked => {
                return broker::Reply::refused(
                    m.reason
                        .as_deref()
                        .unwrap_or("this command isn't available in chats"),
                );
            }
            CliEffect::Local => {
                return broker::Reply::refused("this command runs in the chat, not through tab");
            }
            _ => {}
        }
        let policy = self.hub.policy(session);
        let ask = m.effect == CliEffect::Destructive
            || (m.effect == CliEffect::Write && policy.trust != crate::sandbox::Trust::Person);
        if ask {
            let decision = self
                .hub
                .request_approval(
                    &session.id,
                    tool.name(),
                    format!("Run `{command}`"),
                    json!({ "command": command, "connection": tool.connector }),
                )
                .await;
            if let Decision::Deny(why) = decision {
                return broker::Reply::refused(&format!("the person didn't allow this: {why}"));
            }
        }
        let token = self.engine.cli_token(&tool.connector).await;
        let started = std::time::Instant::now();
        let reply = broker::execute(&tool, &req, &policy, token.as_deref()).await;
        let error = (reply.code != 0).then(|| {
            let e = reply.stderr_text();
            let e = e.trim();
            e.chars()
                .rev()
                .take(500)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect()
        });
        self.log_cli(
            &tool.connector,
            &session.id,
            m.effect,
            &command,
            reply.code,
            started,
            error,
        );
        if reply.code == 0 && matches!(m.effect, CliEffect::Write | CliEffect::Destructive) {
            let repo = broker::repo_of(&req.cwd).await;
            let change = broker::change(&tool, &m, &req.args, repo);
            self.engine
                .pages_changed(&change.connector, change.url.as_slice())
                .await;
            let _ = self.events.send(EngineEvent::CliChanged {
                session_id: session.id.clone(),
                change,
            });
        }
        reply
    }

    pub async fn call(
        &self,
        session: Option<&str>,
        name: &str,
        args: Value,
    ) -> anyhow::Result<Value> {
        let s = |k: &str| {
            args[k]
                .as_str()
                .filter(|v| !v.trim().is_empty())
                .map(str::to_string)
        };
        match name {
            "tab_steps" => {
                let steps = args["steps"]
                    .as_array()
                    .ok_or_else(|| anyhow::anyhow!("steps must be a list"))?;
                let done = steps.iter().filter(|t| t["status"] == "completed").count();
                // One unfinished step is a status line, not a plan: ask for the rest.
                if steps.len() == 1 && done == 0 {
                    return Ok(text(
                        "Shown to the person: 1 step. That's only the step you're on; call tab_steps again \
                         with every step to the finished result (e.g. find it, fix it, check it), so the \
                         person can see the whole plan.",
                    ));
                }
                Ok(text(format!(
                    "Shown to the person: {done} of {} steps done.",
                    steps.len()
                )))
            }
            "tab_search" => {
                let q = s("query").unwrap_or_default();
                let states = if args["include_done"].as_bool().unwrap_or(false) {
                    vec![ItemState::Open, ItemState::Snoozed, ItemState::Done]
                } else {
                    vec![ItemState::Open, ItemState::Snoozed]
                };
                let limit = args["limit"].as_u64().unwrap_or(15).clamp(1, 50) as usize;
                let date = |k: &str| -> anyhow::Result<Option<chrono::DateTime<Utc>>> {
                    let Some(v) = s(k) else { return Ok(None) };
                    if let Ok(t) = chrono::DateTime::parse_from_rfc3339(&v) {
                        return Ok(Some(t.with_timezone(&Utc)));
                    }
                    let d = chrono::NaiveDate::parse_from_str(&v, "%Y-%m-%d").map_err(|_| {
                        anyhow::anyhow!("{k} should be an ISO date like 2026-11-01")
                    })?;
                    Ok(Some(d.and_hms_opt(0, 0, 0).unwrap().and_utc()))
                };
                let now = Utc::now();
                let mut scope = crate::store::Scope {
                    since: date("since")?.or(args["within_days"]
                        .as_i64()
                        .map(|d| now - chrono::Duration::days(d.max(1)))),
                    until: date("until")?,
                    project_id: match s("project").as_deref() {
                        None => None,
                        Some("current") => self.project_arg(session, &json!({}))?.map(|p| p.id),
                        Some(_) => self.project_arg(session, &args)?.map(|p| p.id),
                    },
                };
                let qv = self.store.embed_query(&q);
                let mut items =
                    self.store
                        .search_hybrid(&q, qv.as_deref(), &states, &scope, limit)?;
                // Too little in the window: look further back, a step at a time.
                let mut widened = None;
                if args["expand"].as_bool().unwrap_or(true) {
                    while items.len() < 3.min(limit) {
                        let Some(since) = scope.since else { break };
                        let span =
                            (scope.until.unwrap_or(now) - since).max(chrono::Duration::days(1));
                        let wider = since - span * 3;
                        scope.since = (wider > now - chrono::Duration::days(365)).then_some(wider);
                        items =
                            self.store
                                .search_hybrid(&q, qv.as_deref(), &states, &scope, limit)?;
                        widened = Some(scope.since);
                    }
                }
                let notes = match &qv {
                    Some(v) => self.store.notes_near(v, 3)?,
                    None => Vec::new(),
                };
                let mut out = json!({
                    "items": items.iter().map(brief).collect::<Vec<_>>(),
                    "search": self.store.index_status().summary(),
                });
                if let Some(w) = widened {
                    out["widened_to"] = json!(match w {
                        Some(t) => format!("since {}", t.format("%Y-%m-%d")),
                        None => "all time".into(),
                    });
                }
                if !notes.is_empty() {
                    let names: std::collections::HashMap<String, String> = self
                        .store
                        .projects()?
                        .into_iter()
                        .map(|p| (p.id, p.name))
                        .collect();
                    out["project_notes"] = json!(
                        notes
                            .iter()
                            .map(|n| {
                                let mut v = note_json(n);
                                v["project"] = json!(names.get(&n.project_id));
                                v
                            })
                            .collect::<Vec<_>>()
                    );
                }
                Ok(text(serde_json::to_string_pretty(&out)?))
            }
            "tab_get_item" => {
                let id = s("item_id").ok_or_else(|| anyhow::anyhow!("item_id is required"))?;
                let ctx = self
                    .store
                    .item_context(&id)?
                    .ok_or_else(|| anyhow::anyhow!("no item {id}"))?;
                let preview = self.engine.preview(&id).await.ok();
                Ok(text(serde_json::to_string_pretty(&json!({
                    "item": brief(&ctx.item),
                    "project": ctx.item.project_id.as_deref().and_then(|p| self.store.project_find(p).ok().flatten()).map(|p| p.name),
                    "body": ctx.item.event.body,
                    "preview": preview.map(|p| p.blocks),
                    "context": ctx.entries.iter().map(|c| json!({
                        "section": c.section, "title": c.title, "url": c.url,
                        "summary": c.summary, "item": c.ref_item.as_deref().map(brief),
                    })).collect::<Vec<_>>(),
                    "related": ctx.related.iter().map(brief).collect::<Vec<_>>(),
                    "timeline": self.store.activity_for(&id, 60)?.iter().map(|a| json!({
                        "when": a.at.format("%Y-%m-%d %H:%M UTC").to_string(),
                        "what": a.title, "by": a.by, "on": a.via,
                    })).collect::<Vec<_>>(),
                }))?))
            }
            "tab_add_found" | "tab_attach_context" => {
                let item_id = self.session_item(session, &args)?;
                let ref_item = match s("ref_item_id") {
                    Some(r) => Some(
                        self.store
                            .get(&r)?
                            .ok_or_else(|| anyhow::anyhow!("no item {r}"))?,
                    ),
                    // A link to something in the queue attaches that item.
                    None => s("url").and_then(|u| self.store.item_by_url(&u).ok().flatten()),
                };
                anyhow::ensure!(
                    ref_item.as_ref().is_none_or(|i| i.id != item_id),
                    "that's this task itself; it's already what the chat is about"
                );
                let ref_item_id = ref_item.as_ref().map(|i| i.id.clone());
                let title = s("title")
                    .or(ref_item.as_ref().map(|i| i.event.title.clone()))
                    .or(s("url"))
                    .ok_or_else(|| anyhow::anyhow!("pass a title, url or ref_item_id"))?;
                anyhow::ensure!(
                    ref_item_id.as_deref() != Some(item_id.as_str()),
                    "an item can't be its own context"
                );
                let section = if name == "tab_add_found" {
                    ContextSection::Found
                } else {
                    ContextSection::Context
                };
                let entry = self.store.context_add(
                    &item_id,
                    &NewContext {
                        section,
                        ref_item_id,
                        url: s("url"),
                        title,
                        summary: s("summary"),
                        added_by: "agent".into(),
                    },
                    Utc::now(),
                )?;
                // File the attached item: the project asked for, else (for
                // Context) the chat's project when it has none.
                let mut filed = String::new();
                if let Some(r) = &ref_item {
                    let asked = args["project"]
                        .as_str()
                        .map(str::trim)
                        .filter(|v| !v.is_empty());
                    let moved = match asked {
                        Some(p) if p.eq_ignore_ascii_case("none") => {
                            self.store.item_set_project(&r.id, None, Utc::now())?
                        }
                        Some(p) => {
                            let proj = self
                                .store
                                .project_find(p)?
                                .ok_or_else(|| anyhow::anyhow!("no project named {p}"))?;
                            self.store
                                .item_set_project(&r.id, Some(&proj.id), Utc::now())?
                        }
                        None if section == ContextSection::Context => {
                            self.store.adopt_project(&item_id, &r.id, Utc::now())?
                        }
                        None => None,
                    };
                    let now_in = moved
                        .as_ref()
                        .map_or(r.project_id.clone(), |m| m.project_id.clone());
                    let name = match &now_in {
                        Some(p) => self.store.project_find(p)?.map(|p| p.name),
                        None => None,
                    };
                    filed = match (moved.is_some(), name) {
                        (true, Some(n)) => format!(" Filed it under {n}."),
                        (true, None) => " Took it out of its project.".into(),
                        (false, Some(n)) => format!(" It stays in {n}."),
                        (false, None) => " It has no project.".into(),
                    };
                    if let Some(m) = moved {
                        let _ = self.events.send(EngineEvent::ItemChanged {
                            change: crate::engine::Change::Updated,
                            item: m,
                        });
                    }
                }
                let _ = self.events.send(EngineEvent::ContextChanged { item_id });
                Ok(text(format!(
                    "Added \"{}\" under {}.{filed}",
                    entry.title,
                    entry.section.as_str()
                )))
            }
            "tab_create_task" => {
                let due_at = args["due_in_minutes"]
                    .as_i64()
                    .map(|m| Utc::now() + chrono::Duration::minutes(m.max(1)));
                // Follow-ups land in the same project as the chat's task.
                let project_id = self.project_arg(session, &args)?.map(|p| p.id);
                let item = self
                    .engine
                    .create_task(NewTask {
                        title: s("title").ok_or_else(|| anyhow::anyhow!("title is required"))?,
                        body: s("body"),
                        priority: s("priority"),
                        due_at,
                        url: s("url"),
                        created_by: "agent".into(),
                        project_id,
                        auto_title: false,
                    })
                    .await?;
                Ok(text(format!(
                    "Created task {} ({})",
                    item.id, item.event.kind
                )))
            }
            "tab_graph_search" => {
                let since = args["within_days"]
                    .as_i64()
                    .map(|d| Utc::now() - chrono::Duration::days(d.max(1)));
                let q = s("query").ok_or_else(|| anyhow::anyhow!("query is required"))?;
                let limit = args["limit"].as_u64().unwrap_or(5).clamp(1, 10) as usize;
                let found = self.store.search_entities(&q, limit)?;
                if found.is_empty() {
                    return Ok(text(format!(
                        "Nothing in the graph matches \"{q}\". Try tab_search for words inside items, or ask the person."
                    )));
                }
                let mut out = Vec::new();
                for e in &found {
                    let mut v = entity_json(e);
                    v["connected"] = json!(self.store.co_entities(e.id, None, since, 8)?.iter()
                        .map(|(n, shared)| json!({ "entity_id": n.id, "kind": n.kind, "label": n.label, "shared_items": shared }))
                        .collect::<Vec<_>>());
                    v["recent_items"] = json!(
                        self.store
                            .entity_items(e.id, since, 5)?
                            .iter()
                            .map(brief)
                            .collect::<Vec<_>>()
                    );
                    if let Some(p) = e
                        .key
                        .strip_prefix("project:")
                        .filter(|_| e.kind == "project")
                    {
                        v["notes"] = json!(
                            self.store
                                .project_notes(p)?
                                .iter()
                                .map(note_json)
                                .collect::<Vec<_>>()
                        );
                    }
                    out.push(v);
                }
                Ok(text(serde_json::to_string_pretty(&out)?))
            }
            "tab_graph_expand" => {
                let since = args["within_days"]
                    .as_i64()
                    .map(|d| Utc::now() - chrono::Duration::days(d.max(1)));
                let id = args["entity_id"]
                    .as_i64()
                    .ok_or_else(|| anyhow::anyhow!("entity_id is required"))?;
                let e = self
                    .store
                    .entity(id)?
                    .ok_or_else(|| anyhow::anyhow!("no entity {id}"))?;
                let limit = args["limit"].as_u64().unwrap_or(15).clamp(1, 30) as usize;
                let kind = s("kind");
                Ok(text(serde_json::to_string_pretty(&json!({
                    "entity": entity_json(&e),
                    "connected": self.store.co_entities(id, kind.as_deref(), since, limit)?.iter()
                        .map(|(n, shared)| json!({ "entity_id": n.id, "kind": n.kind, "label": n.label, "shared_items": shared }))
                        .collect::<Vec<_>>(),
                    "recent_items": self.store.entity_items(id, since, 10)?.iter().map(brief).collect::<Vec<_>>(),
                }))?))
            }
            "tab_project" => {
                let since = args["within_days"]
                    .as_i64()
                    .map(|d| Utc::now() - chrono::Duration::days(d.max(1)));
                let Some(p) = self.project_arg(session, &args)? else {
                    let all = self.store.projects()?;
                    return Ok(text(if all.is_empty() {
                        "The person has no projects yet.".to_string()
                    } else {
                        format!(
                            "This chat isn't in a project. Projects: {}",
                            all.iter()
                                .map(|p| p.name.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    }));
                };
                Ok(text(serde_json::to_string_pretty(&json!({
                    "project": { "id": p.id, "name": p.name },
                    "about": p.about,
                    "rules": p.rules,
                    "repos": self.store.project_repos(&p.id)?.iter().map(repo_json).collect::<Vec<_>>(),
                    "notes": self.store.project_notes(&p.id)?.iter().map(note_json).collect::<Vec<_>>(),
                    "items_are_about": self.store.project_entities(&p.id, since, 15)?.iter()
                        .map(|(e, n)| json!({ "entity_id": e.id, "kind": e.kind, "label": e.label, "items_in_project": n }))
                        .collect::<Vec<_>>(),
                    "recent_items": self.store.project_items(&p.id, since, 15)?.iter().map(brief).collect::<Vec<_>>(),
                }))?))
            }
            "tab_set_project" => {
                let item_id = self.session_item(session, &args)?;
                let project = match args["project"].as_str().filter(|v| !v.trim().is_empty()) {
                    None => None,
                    Some(name) => Some(self.store.project_find(name)?.ok_or_else(|| {
                        let names: Vec<_> = self.store.projects().unwrap_or_default().into_iter().map(|p| p.name).collect();
                        anyhow::anyhow!(
                            "no project named {name}. Projects: {}. The person creates new ones in tab.",
                            if names.is_empty() { "none yet".to_string() } else { names.join(", ") }
                        )
                    })?),
                };
                let item = self
                    .store
                    .item_set_project(
                        &item_id,
                        project.as_ref().map(|p| p.id.as_str()),
                        Utc::now(),
                    )?
                    .ok_or_else(|| anyhow::anyhow!("no item {item_id}"))?;
                let _ = self.events.send(EngineEvent::ItemChanged {
                    change: crate::engine::Change::Updated,
                    item: item.clone(),
                });
                Ok(text(match project {
                    Some(p) => format!("Moved \"{}\" into {}.", item.event.title, p.name),
                    None => format!("Took \"{}\" out of its project.", item.event.title),
                }))
            }
            "tab_project_update" => {
                let p = self.project_arg(session, &args)?.ok_or_else(|| {
                    anyhow::anyhow!(
                        "this chat isn't in a project; pass project (a name from tab_project)"
                    )
                })?;
                let (about, rules) = (args["about"].as_str(), args["rules"].as_str());
                anyhow::ensure!(
                    about.is_some() || rules.is_some(),
                    "pass about, rules or both"
                );
                self.store
                    .project_set_profile(&p.id, about, rules, Utc::now())?;
                let _ = self.events.send(EngineEvent::ProjectChanged {
                    project_id: p.id.clone(),
                });
                let what = match (about.is_some(), rules.is_some()) {
                    (true, true) => "about and rules",
                    (true, false) => "about",
                    _ => "rules",
                };
                Ok(text(format!("Saved {}'s {what}.", p.name)))
            }
            "tab_project_repo" => {
                let repo = s("repo").ok_or_else(|| anyhow::anyhow!("repo is required"))?;
                let p = self.project_arg(session, &args)?.ok_or_else(|| {
                    anyhow::anyhow!(
                        "this chat isn't in a project; pass project (a name from tab_project)"
                    )
                })?;
                let msg = if args["remove"].as_bool().unwrap_or(false) {
                    if !self.store.repo_delete(&p.id, &repo)? {
                        return Ok(text(format!("{} doesn't list {repo}.", p.name)));
                    }
                    format!("Removed {repo} from {}.", p.name)
                } else {
                    let r = self.store.repo_save(
                        &p.id,
                        &repo,
                        args["path"].as_str(),
                        args["note"].as_str(),
                        "agent",
                        Utc::now(),
                    )?;
                    format!("{} lists {}.", p.name, r.repo)
                };
                let _ = self.events.send(EngineEvent::ProjectChanged {
                    project_id: p.id.clone(),
                });
                Ok(text(msg))
            }
            "tab_project_note" => {
                let topic = s("topic").ok_or_else(|| anyhow::anyhow!("topic is required"))?;
                let p = self.project_arg(session, &args)?.ok_or_else(|| {
                    anyhow::anyhow!(
                        "this chat isn't in a project; pass project (a name from tab_project)"
                    )
                })?;
                if args["delete"].as_bool().unwrap_or(false) {
                    let gone = self
                        .store
                        .project_notes(&p.id)?
                        .into_iter()
                        .find(|n| n.topic.eq_ignore_ascii_case(&topic));
                    let Some(n) = gone else {
                        return Ok(text(format!("{} has no note on \"{topic}\".", p.name)));
                    };
                    self.store.note_delete(&n.id)?;
                    let _ = self.events.send(EngineEvent::ProjectChanged {
                        project_id: p.id.clone(),
                    });
                    return Ok(text(format!(
                        "Removed the \"{topic}\" note from {}.",
                        p.name
                    )));
                }
                let body = s("text").ok_or_else(|| anyhow::anyhow!("text is required"))?;
                let sources: Vec<String> = args["source_item_ids"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                let n = self.store.note_save(
                    &p.id,
                    &topic,
                    &body,
                    "agent",
                    session,
                    &sources,
                    Utc::now(),
                )?;
                let _ = self.events.send(EngineEvent::ProjectChanged {
                    project_id: p.id.clone(),
                });
                Ok(text(format!(
                    "Saved the \"{}\" note on {}.",
                    n.topic, p.name
                )))
            }
            "tab_list_connections" => {
                let mut out = Vec::new();
                for c in self.engine.connectors().await? {
                    let saved = self.engine.connector_config(&c.descriptor.id).await?;
                    let fields: Vec<_> = c
                        .descriptor
                        .settings
                        .iter()
                        .map(|f| {
                            let v = saved.as_ref().and_then(|s| s.settings.get(&f.key));
                            let value = if f.kind == SettingKind::Secret {
                                json!(
                                    if v.and_then(|v| v.as_str()).is_some_and(|s| !s.is_empty()) {
                                        "(set)"
                                    } else {
                                        "(not set)"
                                    }
                                )
                            } else {
                                v.cloned().unwrap_or(Value::Null)
                            };
                            json!({ "key": f.key, "label": f.label, "kind": f.kind,
                                    "required": f.required, "help": f.help, "value": value })
                        })
                        .collect();
                    out.push(json!({
                        "id": c.descriptor.id,
                        "name": c.descriptor.name,
                        "description": c.descriptor.description,
                        "available": c.descriptor.available,
                        "installed": c.enabled,
                        "status": c.status,
                        "settings": fields,
                    }));
                }
                Ok(text(serde_json::to_string_pretty(&out)?))
            }
            "tab_find_email_server" => {
                let address = s("address").ok_or_else(|| anyhow::anyhow!("address is required"))?;
                let found = crate::connectors::email::discover(&address).await?;
                Ok(text(serde_json::to_string_pretty(&found)?))
            }
            "tab_configure_connection" => {
                let id =
                    s("connector_id").ok_or_else(|| anyhow::anyhow!("connector_id is required"))?;
                let info = self.connector_info(&id).await?;
                let d = &info.descriptor;
                anyhow::ensure!(d.available, "{} isn't built yet", d.name);
                let mut cfg = self.engine.connector_config(&id).await?.unwrap_or_default();
                let given = args["settings"].as_object().cloned().unwrap_or_default();
                for (k, v) in given {
                    let field = d.settings.iter().find(|f| f.key == k).ok_or_else(|| {
                        anyhow::anyhow!("{} has no setting {k}; see tab_list_connections", d.name)
                    })?;
                    anyhow::ensure!(
                        field.kind != SettingKind::Secret,
                        "Don't pass {}: the person types secrets into the card from tab_ask_for_secret.",
                        field.label
                    );
                    cfg.settings.insert(k, v);
                }
                let missing = missing_required(&info, &cfg);
                let enable = args["enable"].as_bool().unwrap_or(true);
                cfg.enabled = enable && missing.is_empty();
                self.engine.save_connector(&id, cfg.clone()).await?;
                let secrets: Vec<_> = d
                    .settings
                    .iter()
                    .filter(|f| f.kind == SettingKind::Secret && missing.contains(&f.label))
                    .collect();
                if !secrets.is_empty() {
                    self.ask_secret(session, &info, None);
                    return Ok(text(format!(
                        "Saved. {} still needs {} from the person: a card is now in the chat for them to type it. \
                         Tell them it's there, then stop and wait for their next message before testing.",
                        d.name,
                        missing.join(", ")
                    )));
                }
                if !missing.is_empty() {
                    return Ok(text(format!(
                        "Saved, but not installed yet: still missing {}.",
                        missing.join(", ")
                    )));
                }
                Ok(text(if cfg.enabled {
                    format!(
                        "Saved and installed {}. Call tab_test_connection to check it.",
                        d.name
                    )
                } else {
                    format!("Saved {} without installing it.", d.name)
                }))
            }
            "tab_ask_for_secret" => {
                let id =
                    s("connector_id").ok_or_else(|| anyhow::anyhow!("connector_id is required"))?;
                let info = self.connector_info(&id).await?;
                anyhow::ensure!(
                    info.descriptor
                        .settings
                        .iter()
                        .any(|f| f.kind == SettingKind::Secret),
                    "{} has no secrets to enter",
                    info.descriptor.name
                );
                self.ask_secret(session, &info, s("reason"));
                Ok(text(
                    "The card is in the chat. Stop and wait for the person to say they've entered it.",
                ))
            }
            "tab_test_connection" => {
                let id =
                    s("connector_id").ok_or_else(|| anyhow::anyhow!("connector_id is required"))?;
                let info = self.connector_info(&id).await?;
                let cfg = self.engine.connector_config(&id).await?.unwrap_or_default();
                let missing = missing_required(&info, &cfg);
                anyhow::ensure!(missing.is_empty(), "Not set yet: {}", missing.join(", "));
                if id == "email" {
                    return Ok(text(match crate::connectors::email::check(&cfg).await {
                        Ok(report) => format!(
                            "{report}{}",
                            if cfg.enabled {
                                "The connection is installed and syncing."
                            } else {
                                "Signed in, but the connection is off: call tab_configure_connection with enable true."
                            }
                        ),
                        Err(e) => format!("Sign-in failed: {e:#}"),
                    }));
                }
                Ok(text(format!(
                    "{} has no live test; its status is {}",
                    info.descriptor.name,
                    serde_json::to_string(&info.status)?
                )))
            }
            "approve" => {
                let tool = s("tool_name").unwrap_or_else(|| "tool".into());
                let input = args["input"].clone();
                let session =
                    session.ok_or_else(|| anyhow::anyhow!("approvals need a tab session"))?;
                let decision = self
                    .hub
                    .request_approval(session, &tool, summarize_tool(&tool, &input), input.clone())
                    .await;
                let verdict = match decision {
                    // AskUserQuestion gets the person's picks back in its input.
                    Decision::Answer(answers) => {
                        let mut input = input;
                        if let Some(o) = input.as_object_mut() {
                            o.insert("answers".into(), answers);
                        }
                        json!({ "behavior": "allow", "updatedInput": input })
                    }
                    Decision::Allow | Decision::AllowAlways => {
                        json!({ "behavior": "allow", "updatedInput": input })
                    }
                    Decision::Deny(m) => json!({ "behavior": "deny", "message": m }),
                };
                Ok(text(verdict.to_string()))
            }
            "tab_list_actions" => {
                let only = s("connector_id");
                let changes = [
                    "ingested", "updated", "ranked", "snoozed", "woke", "done", "reopened",
                ];
                let connections: Vec<Value> = self
                    .catalog()
                    .await
                    .into_iter()
                    .filter(|c| only.as_ref().is_none_or(|o| *o == c.descriptor.id))
                    .map(|c| {
                        json!({
                            "id": c.descriptor.id,
                            "name": c.descriptor.name,
                            "installed": c.enabled,
                            "item_kinds": c.descriptor.kinds.iter().map(|k| json!({ "kind": k.kind, "label": k.label })).collect::<Vec<_>>(),
                            "actions": c.descriptor.actions,
                        })
                    })
                    .collect();
                Ok(text(serde_json::to_string_pretty(&json!({
                    "item_changes": changes,
                    "connections": connections,
                    "templates": {
                        "item": "The item that started the run: item.id, item.title, item.body, item.kind, item.source, item.actor, item.context, item.url, item.score, item.urgency, item.state, item.metadata.<key> (GitHub: repo, number, reason, subject_type; email: folder, uid).",
                        "steps": "steps.<step id>.<output key> for an earlier step's output.",
                        "signal": "signal.<key> for a signal trigger's payload.",
                        "trigger": "trigger.type, trigger.change.",
                        "now": "The time, RFC 3339.",
                    },
                    "rules": "Effects: read runs in dry runs too; write changes something; destructive (merge, close, move mail) stays blocked until the person allows it on that workflow. Command steps run a program in tab's sandbox: arguments are passed without a shell, it can change only its folder (a fresh one per run when empty), reach only the hosts in network (empty means none), and gets none of tab's secrets; with image it runs in a Docker or Podman container. Commands stay blocked until the person allows commands on that workflow; outputs are steps.<id>.exit_code, stdout and stderr. A workflow can't start itself again through its own changes, chains stop after 3 workflows, the same trigger runs once, and each workflow has an hourly run limit.",
                }))?))
            }
            "tab_list_workflows" => {
                let all = self.store.workflows()?;
                let one = s("workflow_id");
                let out: Vec<Value> = all
                    .iter()
                    .filter(|w| one.as_ref().is_none_or(|o| *o == w.id))
                    .map(|w| {
                        let runs = self
                            .store
                            .workflow_runs(Some(&w.id), if one.is_some() { 10 } else { 3 })
                            .unwrap_or_default();
                        json!({
                            "summary": workflow::describe_trigger(&w.trigger),
                            "definition": w,
                            "recent_runs": runs.iter().map(|r| json!({
                                "status": r.status, "trigger": r.trigger, "dry_run": r.dry_run, "error": r.error,
                                "when": r.started_at.format("%Y-%m-%d %H:%M UTC").to_string(),
                                "steps": r.steps.iter().map(|s| json!({ "id": s.id, "status": s.status, "error": s.error })).collect::<Vec<_>>(),
                            })).collect::<Vec<_>>(),
                        })
                    })
                    .collect();
                Ok(text(serde_json::to_string_pretty(&out)?))
            }
            "tab_save_workflow" => {
                let w: Workflow = serde_json::from_value(args["workflow"].clone())
                    .map_err(|e| anyhow::anyhow!("that isn't a workflow definition: {e}"))?;
                let saved = self.store.workflows()?;
                if !w.id.is_empty() && !saved.iter().any(|o| o.id == w.id) {
                    anyhow::bail!("no workflow {}; leave id out to create one", w.id);
                }
                let catalog = self.catalog().await;
                let (w, issues) = workflow::prepare_save(w, SavedBy::Agent, &catalog, &saved);
                let errors: Vec<_> = issues.iter().filter(|i| i.error).collect();
                if !errors.is_empty() {
                    let list: Vec<String> = errors
                        .iter()
                        .map(|i| match &i.at {
                            Some(a) => format!("- {a}: {}", i.message),
                            None => format!("- {}", i.message),
                        })
                        .collect();
                    anyhow::bail!("Not saved. Fix these and save again:\n{}", list.join("\n"));
                }
                let created = w.id.is_empty();
                let w = self.store.workflow_save(&w, "agent", Utc::now())?;
                // A new draft's builder chat follows it to its id.
                if created
                    && let Some(sid) = session.filter(|s| !s.is_empty())
                    && self.store.session_key(sid)?.as_deref() == Some(&*workflow::chat_key(""))
                {
                    self.store
                        .session_set_key(sid, &workflow::chat_key(&w.id))?;
                }
                let _ = self.events.send(EngineEvent::WorkflowsChanged {
                    id: w.id.clone(),
                    by: "agent".into(),
                });
                let warnings: Vec<String> =
                    issues.iter().map(|i| format!("- {}", i.message)).collect();
                Ok(text(format!(
                    "Saved \"{}\" (id {}, version {}), turned off. It's open in the person's Workflows tab; tell them what it does and that they turn it on there.{}",
                    w.name,
                    w.id,
                    w.version,
                    if warnings.is_empty() {
                        String::new()
                    } else {
                        format!("\nWorth mentioning:\n{}", warnings.join("\n"))
                    }
                )))
            }
            "tab_test_workflow" => {
                let w: Workflow = match (
                    s("workflow_id"),
                    args.get("workflow").filter(|v| v.is_object()),
                ) {
                    (Some(id), _) => self
                        .store
                        .workflow_get(&id)?
                        .ok_or_else(|| anyhow::anyhow!("no workflow {id}"))?,
                    (None, Some(def)) => {
                        let mut w: Workflow = serde_json::from_value(def.clone()).map_err(|e| {
                            anyhow::anyhow!("that isn't a workflow definition: {e}")
                        })?;
                        workflow::fill_defaults(&mut w, &self.catalog().await);
                        w
                    }
                    (None, None) => anyhow::bail!("pass workflow_id or workflow"),
                };
                let run = self.engine.dry_run_workflow(w, s("item_id")).await?;
                Ok(text(serde_json::to_string_pretty(&json!({
                    "status": run.status,
                    "error": run.error,
                    "steps": run.steps,
                }))?))
            }
            _ => anyhow::bail!("unknown tool {name}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AgentConfig;
    use crate::model::{AgentKind, IncomingEvent};

    /// Where the git endpoint forwards to in tests, instead of github.com.
    pub(super) static UPSTREAM: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

    struct NoEngine(Arc<Store>);

    #[async_trait]
    impl EngineAccess for NoEngine {
        async fn cli_token(&self, _: &str) -> Option<String> {
            Some("conn-token".into())
        }
        async fn create_task(&self, t: NewTask) -> anyhow::Result<QueueItem> {
            let e = IncomingEvent {
                source: "tab".into(),
                external_id: t.title.clone(),
                kind: "tab.task".into(),
                title: t.title,
                occurred_at: Utc::now(),
                ..Default::default()
            };
            match self.0.upsert(&e, Utc::now())? {
                crate::store::Upsert::Inserted(i) => Ok(i),
                _ => anyhow::bail!("not inserted"),
            }
        }
        async fn preview(&self, id: &str) -> anyhow::Result<Preview> {
            Ok(Preview {
                item_id: id.into(),
                blocks: vec![],
                fetched_at: None,
                stale: false,
            })
        }
    }

    #[tokio::test]
    async fn speaks_mcp_over_http_and_files_context() {
        let store = Arc::new(Store::in_memory().unwrap());
        let (events, _) = tokio::sync::broadcast::channel(64);
        let hub = AgentHub::new(
            store.clone(),
            events.clone(),
            AgentConfig::default(),
            std::env::temp_dir(),
        );
        let server = Arc::new(McpServer {
            store: store.clone(),
            hub: hub.clone(),
            engine: Arc::new(NoEngine(store.clone())),
            events,
            token: "secret".into(),
        });
        let url = serve(server, 47_900).await.unwrap();
        let client = reqwest::Client::new();
        let rpc = |body: Value, token: &str| {
            client
                .post(&url)
                .bearer_auth(token)
                .header("x-tab-session", "")
                .json(&body)
                .send()
        };

        assert_eq!(rpc(json!({}), "wrong").await.unwrap().status(), 401);
        let init: Value = rpc(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}), "secret")
            .await.unwrap().json().await.unwrap();
        assert_eq!(init["result"]["serverInfo"]["name"], "tab");
        let note = rpc(
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            "secret",
        )
        .await
        .unwrap();
        assert_eq!(note.status(), 202);
        let list: Value = rpc(
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
            "secret",
        )
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
        assert_eq!(list["result"]["tools"].as_array().unwrap().len(), 23);
        let always: Vec<&str> = list["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|t| t["_meta"]["anthropic/alwaysLoad"] == true)
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(always.len(), ALWAYS_LOAD.len());
        assert!(always.contains(&"tab_steps"));

        // Create a task, a ticket to find, then file it under Found on the task.
        let call = |name: &str, args: Value| json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":name,"arguments":args}});
        let created: Value = rpc(
            call("tab_create_task", json!({"title":"Fix checkout"})),
            "secret",
        )
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
        let task_text = created["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        let task_id = task_text.split_whitespace().nth(2).unwrap().to_string();
        let ticket = NoEngine(store.clone())
            .create_task(NewTask {
                title: "PAY-12 checkout 500s".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        let session = store
            .session_create(Some(&task_id), AgentKind::ClaudeCode, "t", ".", Utc::now())
            .unwrap();
        let found = server_call(
            &url,
            &session.id,
            call(
                "tab_add_found",
                json!({"ref_item_id": ticket.id, "summary": "Same stack trace"}),
            ),
        )
        .await;
        assert!(
            found["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("under found")
        );
        let ctx = store.item_context(&task_id).unwrap().unwrap();
        assert_eq!(ctx.entries.len(), 1);
        assert_eq!(ctx.entries[0].section, ContextSection::Found);

        let search = server_call(
            &url,
            &session.id,
            call("tab_search", json!({"query":"checkout"})),
        )
        .await;
        assert!(
            search["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("PAY-12")
        );
    }

    /// An engine with connections kept in memory.
    struct Connections(std::sync::Mutex<std::collections::HashMap<String, ConnectorConfig>>);

    #[async_trait]
    impl EngineAccess for Connections {
        async fn create_task(&self, _: NewTask) -> anyhow::Result<QueueItem> {
            anyhow::bail!("unused")
        }
        async fn preview(&self, _: &str) -> anyhow::Result<Preview> {
            anyhow::bail!("unused")
        }
        async fn connectors(&self) -> anyhow::Result<Vec<ConnectorInfo>> {
            let saved = self.0.lock().unwrap();
            Ok(crate::connectors::registry()
                .into_iter()
                .map(|f| {
                    let descriptor = f.descriptor();
                    ConnectorInfo {
                        enabled: saved.get(&descriptor.id).is_some_and(|c| c.enabled),
                        descriptor,
                        status: crate::model::ConnectorStatus::Disabled,
                    }
                })
                .collect())
        }
        async fn connector_config(&self, id: &str) -> anyhow::Result<Option<ConnectorConfig>> {
            Ok(self.0.lock().unwrap().get(id).cloned())
        }
        async fn save_connector(&self, id: &str, cfg: ConnectorConfig) -> anyhow::Result<()> {
            self.0.lock().unwrap().insert(id.into(), cfg);
            Ok(())
        }
    }

    #[tokio::test]
    async fn sets_up_a_connection_without_seeing_the_password() {
        let store = Arc::new(Store::in_memory().unwrap());
        let (events, _) = tokio::sync::broadcast::channel(64);
        let hub = AgentHub::new(
            store.clone(),
            events.clone(),
            AgentConfig::default(),
            std::env::temp_dir(),
        );
        let engine = Arc::new(Connections(Default::default()));
        let server = McpServer {
            store: store.clone(),
            hub,
            engine: engine.clone(),
            events,
            token: "t".into(),
        };
        let session = store
            .session_create(None, AgentKind::ClaudeCode, "t", ".", Utc::now())
            .unwrap();
        let out = |v: anyhow::Result<Value>| {
            v.map(|v| v["content"][0]["text"].as_str().unwrap().to_string())
        };

        // Secrets can't be passed by the agent.
        let refused = out(server
            .call(
                Some(&session.id),
                "tab_configure_connection",
                json!({"connector_id": "email", "settings": {"password": "x"}}),
            )
            .await);
        assert!(refused.unwrap_err().to_string().contains("card"));

        let saved = out(server
            .call(
                Some(&session.id),
                "tab_configure_connection",
                json!({"connector_id": "email",
                "settings": {"host": "heracles.mxrouting.net", "username": "v@x.dev"}}),
            )
            .await)
        .unwrap();
        assert!(saved.contains("Password"), "{saved}");
        let cfg = engine.0.lock().unwrap()["email"].clone();
        assert!(!cfg.enabled, "not on until the password is in");
        assert_eq!(cfg.str("host"), Some("heracles.mxrouting.net"));

        // The card is in the chat, with the field to type into.
        let card = store.messages(&session.id).unwrap().pop().unwrap();
        assert_eq!(card.kind, "secret_request");
        assert_eq!(card.meta["fields"][0]["key"], "password");

        let listed = out(server.call(None, "tab_list_connections", json!({})).await).unwrap();
        assert!(listed.contains("(not set)") && listed.contains("heracles"));

        // Once the person typed it, it shows as set, never as the value.
        engine
            .0
            .lock()
            .unwrap()
            .get_mut("email")
            .unwrap()
            .settings
            .insert("password".into(), json!("hunter2"));
        let listed = out(server.call(None, "tab_list_connections", json!({})).await).unwrap();
        assert!(listed.contains("(set)") && !listed.contains("hunter2"));

        let bad = out(server
            .call(
                None,
                "tab_configure_connection",
                json!({"connector_id": "email", "settings": {"nope": 1}}),
            )
            .await);
        assert!(bad.is_err());
    }

    #[tokio::test]
    async fn agents_search_the_graph_and_leave_project_notes() {
        use crate::model::{EntityRef, IncomingEvent};
        let store = Arc::new(Store::in_memory().unwrap());
        let (events, _) = tokio::sync::broadcast::channel(64);
        let hub = AgentHub::new(
            store.clone(),
            events.clone(),
            AgentConfig::default(),
            std::env::temp_dir(),
        );
        let server = McpServer {
            store: store.clone(),
            hub,
            engine: Arc::new(Connections(Default::default())),
            events,
            token: "t".into(),
        };
        let now = Utc::now();
        let p = store
            .project_save(None, "Sidequest", "#7c5cff", "zap", now)
            .unwrap();
        let ev = |id: &str, title: &str| IncomingEvent {
            source: "github".into(),
            external_id: id.into(),
            kind: "github.pr".into(),
            title: title.into(),
            occurred_at: now,
            entities: vec![
                EntityRef::new("repo", "github:acme/sidequest", "acme/sidequest", "in"),
                EntityRef::new("person", "github:dana", "Dana", "from"),
            ],
            ..Default::default()
        };
        let crate::store::Upsert::Inserted(a) =
            store.upsert(&ev("1", "Add billing page"), now).unwrap()
        else {
            panic!()
        };
        store.upsert(&ev("2", "Fix login"), now).unwrap();
        store.item_set_project(&a.id, Some(&p.id), now).unwrap();
        let session = store
            .session_create(Some(&a.id), AgentKind::ClaudeCode, "t", ".", now)
            .unwrap();
        let out = |v: anyhow::Result<Value>| {
            v.map(|v| v["content"][0]["text"].as_str().unwrap().to_string())
                .unwrap()
        };

        // "sidequest" finds the project and the repo, each with its neighbours.
        let found = out(server
            .call(
                Some(&session.id),
                "tab_graph_search",
                json!({"query":"sidequest"}),
            )
            .await);
        let found: Value = serde_json::from_str(&found).unwrap();
        let kinds: Vec<_> = found
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["kind"].as_str().unwrap())
            .collect();
        assert!(
            kinds.contains(&"project") && kinds.contains(&"repo"),
            "{kinds:?}"
        );
        let repo = found
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["kind"] == "repo")
            .unwrap();
        assert_eq!(repo["recent_items"].as_array().unwrap().len(), 2);
        let expanded = out(server
            .call(
                None,
                "tab_graph_expand",
                json!({"entity_id": repo["entity_id"], "kind":"person"}),
            )
            .await);
        assert!(expanded.contains("Dana"));

        // A note defaults to the chat's project and replaces by topic.
        out(server
            .call(
                Some(&session.id),
                "tab_project_note",
                json!({"topic":"deploys","text":"Via staging.","source_item_ids":[a.id]}),
            )
            .await);
        out(server
            .call(
                Some(&session.id),
                "tab_project_note",
                json!({"topic":"Deploys","text":"Via staging, Fridays to prod."}),
            )
            .await);
        let notes = store.project_notes(&p.id).unwrap();
        assert_eq!(notes.len(), 1, "same topic, any case, replaces");
        let overview = out(server
            .call(Some(&session.id), "tab_project", json!({}))
            .await);
        assert!(
            overview.contains("Fridays")
                && overview.contains("acme/sidequest")
                && overview.contains("Add billing page")
        );
        // The next chat in the project starts with the notes.
        assert!(server.hub.system_prompt(&session).contains("Fridays"));

        // About, rules and repos: written by agents, read back by tab_project.
        out(server
            .call(
                Some(&session.id),
                "tab_project_update",
                json!({"about":"The billing site.","rules":"- PRs against main."}),
            )
            .await);
        out(server
            .call(Some(&session.id), "tab_project_repo", json!({"repo":"https://github.com/acme/billing","path":"/src/billing","note":"The site"}))
            .await);
        let overview = out(server
            .call(Some(&session.id), "tab_project", json!({}))
            .await);
        assert!(
            overview.contains("The billing site.")
                && overview.contains("PRs against main")
                && overview.contains("\"acme/billing\"")
                && overview.contains("/src/billing"),
            "{overview}"
        );
        out(server
            .call(
                Some(&session.id),
                "tab_project_repo",
                json!({"repo":"acme/billing","remove":true}),
            )
            .await);
        assert!(store.project_repos(&p.id).unwrap().is_empty());
        out(server
            .call(
                Some(&session.id),
                "tab_project_note",
                json!({"topic":"deploys","delete":true}),
            )
            .await);
        assert!(store.project_notes(&p.id).unwrap().is_empty());
    }

    fn git(dir: &std::path::Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "init.defaultBranch=main",
            ])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    #[tokio::test]
    async fn chats_run_git_and_gh_through_tab() {
        if which::which("git").is_err() {
            return;
        }
        let root = std::env::temp_dir().join(format!("tab-cli-{}", uuid::Uuid::new_v4()));
        let (origin, work) = (root.join("origin.git"), root.join("work"));
        std::fs::create_dir_all(&origin).unwrap();
        std::fs::create_dir_all(&work).unwrap();
        git(&origin, &["init", "--bare", "-q"]);
        git(&work, &["init", "-q"]);
        std::fs::write(work.join("a.txt"), "a").unwrap();
        git(&work, &["add", "."]);
        git(&work, &["commit", "-qm", "first"]);

        let store = Arc::new(Store::in_memory().unwrap());
        let (events, _) = tokio::sync::broadcast::channel(64);
        let mut rx = events.subscribe();
        let hub = AgentHub::new(
            store.clone(),
            events.clone(),
            AgentConfig::default(),
            root.clone(),
        );
        let server = Arc::new(McpServer {
            store: store.clone(),
            hub: hub.clone(),
            engine: Arc::new(NoEngine(store.clone())),
            events,
            token: "secret".into(),
        });
        let url = serve(server, 47_960).await.unwrap().replace("/mcp", "/cli");
        let session = store
            .session_create(
                None,
                AgentKind::ClaudeCode,
                "t",
                &work.display().to_string(),
                Utc::now(),
            )
            .unwrap();
        let send = |args: &[&str], id: &str| {
            let req = broker::Request {
                tool: args[0].into(),
                args: args[1..].iter().map(|a| a.to_string()).collect(),
                cwd: work.clone(),
                stdin: None,
            };
            let (url, id) = (url.clone(), id.to_string());
            let token = chat_token("secret", &id);
            tokio::task::spawn_blocking(move || broker::send(&url, &token, &id, &req))
        };

        // Only tab's own chats.
        assert!(
            send(&["git", "ls-remote", "x"], "nobody")
                .await
                .unwrap()
                .is_err()
        );
        // tab's own token isn't a chat's, and one chat's token isn't another's.
        for (token, id) in [
            ("secret".to_string(), session.id.clone()),
            (chat_token("secret", "other"), session.id.clone()),
        ] {
            let req = broker::Request {
                tool: "gh".into(),
                args: vec!["pr".into(), "list".into()],
                cwd: work.clone(),
                stdin: None,
            };
            let u = url.clone();
            let r = tokio::task::spawn_blocking(move || broker::send(&u, &token, &id, &req))
                .await
                .unwrap();
            assert!(r.unwrap_err().to_string().contains("401"));
        }

        // git runs in the chat; only its traffic to GitHub goes through tab.
        let o = origin.display().to_string();
        let r = send(&["git", "push", &o, "HEAD:refs/heads/main"], &session.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(r.code, 1);
        assert!(
            r.stderr_text().contains("runs in the chat"),
            "{}",
            r.stderr_text()
        );

        // gh never hands out the token.
        let r = send(&["gh", "auth", "token"], &session.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(r.code, 1);
        assert!(r.stderr_text().contains("gh auth"), "{}", r.stderr_text());

        // Merging asks the person first; a no stops it, and it's logged.
        let pending = send(&["gh", "pr", "merge", "12", "--squash"], &session.id);
        let id = loop {
            if let EngineEvent::ApprovalRequested { approval } = rx.recv().await.unwrap() {
                assert_eq!(approval.summary, "Run `gh pr merge 12 --squash`");
                break approval.id;
            }
        };
        hub.resolve_approval(&id, Decision::Deny("not now".into()));
        let r = pending.await.unwrap().unwrap();
        assert_eq!(r.code, 1);
        assert!(
            r.stderr_text().contains("didn't allow"),
            "{}",
            r.stderr_text()
        );

        // git credentials: never for GitHub, whose traffic goes through tab.
        let req = broker::Request {
            tool: "git-credential".into(),
            args: vec!["get".into()],
            cwd: work.clone(),
            stdin: Some({
                use base64::Engine as _;
                base64::engine::general_purpose::STANDARD
                    .encode("protocol=https\nhost=github.com\n\n")
            }),
        };
        let (u, id) = (url.clone(), session.id.clone());
        let token = chat_token("secret", &id);
        let r = tokio::task::spawn_blocking(move || broker::send(&u, &token, &id, &req))
            .await
            .unwrap()
            .unwrap();
        assert_eq!((r.code, r.stdout_text()), (0, String::new()));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn chats_may_rewrite_only_branches_they_created_and_alone_pushed_to() {
        let store = Store::in_memory().unwrap();
        let mk = || {
            store
                .session_create(None, AgentKind::ClaudeCode, "t", ".", Utc::now())
                .unwrap()
                .id
        };
        let (me, other) = (mk(), mk());
        let up = |old: &str, new: &str, name: &str| broker::RefUpdate {
            old: old.repeat(40),
            new: new.repeat(40),
            name: name.into(),
        };
        let mine = "refs/heads/mine";

        // A branch someone else made isn't the chat's, even after it pushes to it.
        remember_branches(&store, &me, "o/r", &[up("1", "2", "refs/heads/main")]);
        assert!(!own_branch(
            &store,
            &me,
            "o/r",
            &up("2", "3", "refs/heads/main")
        ));

        // One it created is, for that chat and repo only.
        remember_branches(&store, &me, "o/r", &[up("0", "a", mine)]);
        assert!(own_branch(&store, &me, "O/R", &up("a", "b", mine)));
        assert!(!own_branch(&store, &other, "o/r", &up("a", "b", mine)));
        assert!(!own_branch(&store, &me, "o/other", &up("a", "b", mine)));

        // It follows the chat's own pushes, force pushes included.
        remember_branches(&store, &me, "o/r", &[up("a", "b", mine)]);
        assert!(own_branch(&store, &me, "o/r", &up("b", "c", mine)));

        // Someone else pushed since (the branch isn't where the chat left it): ask.
        assert!(!own_branch(&store, &me, "o/r", &up("d", "c", mine)));

        // Deleting it forgets it.
        remember_branches(&store, &me, "o/r", &[up("b", "0", mine)]);
        assert!(!own_branch(&store, &me, "o/r", &up("b", "c", mine)));
    }

    #[tokio::test]
    async fn chat_git_reaches_github_through_tab_without_the_token() {
        if which::which("git").is_err() {
            return;
        }
        // A stand-in for github.com that records what it was sent.
        let seen: Arc<std::sync::Mutex<Vec<(String, String)>>> = Default::default();
        let rec = seen.clone();
        let fake = Router::new().fallback(move |uri: axum::http::Uri, h: HeaderMap| {
            let rec = rec.clone();
            async move {
                let auth = h
                    .get("authorization")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_string();
                rec.lock().unwrap().push((uri.to_string(), auth));
                StatusCode::NOT_FOUND
            }
        });
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let fake_url = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, fake).await.unwrap() });
        *UPSTREAM.lock().unwrap() = Some(fake_url);

        let root = std::env::temp_dir().join(format!("tab-git-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-q"]);
        let store = Arc::new(Store::in_memory().unwrap());
        let (events, _) = tokio::sync::broadcast::channel(64);
        let mut rx = events.subscribe();
        let hub = AgentHub::new(
            store.clone(),
            events.clone(),
            AgentConfig::default(),
            root.clone(),
        );
        let server = Arc::new(McpServer {
            store: store.clone(),
            hub: hub.clone(),
            engine: Arc::new(NoEngine(store.clone())),
            events,
            token: "secret".into(),
        });
        let mcp = serve(server, 47_980).await.unwrap();
        let session = store
            .session_create(
                None,
                AgentKind::ClaudeCode,
                "t",
                &root.display().to_string(),
                Utc::now(),
            )
            .unwrap();
        let base = mcp.replace("/mcp", &format!("/git/{}/", session.id));

        // git in the chat: a github.com remote goes to tab, with tab's
        // token, and tab sends GitHub the connection's.
        let token = chat_token("secret", &session.id);
        let env = broker::git_config(&base, &token, std::path::Path::new("/nonexistent"));
        let out = tokio::process::Command::new("git")
            .args(["ls-remote", "git@github.com:o/r.git"])
            .current_dir(&root)
            .envs(env)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .await
            .unwrap();
        assert!(!out.status.success());
        let got = seen.lock().unwrap().clone();
        assert!(
            got.iter().any(|(u, _)| u.starts_with("/o/r.git/info/refs")),
            "{got:?}"
        );
        use base64::Engine as _;
        let want = format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode("x-access-token:conn-token")
        );
        assert!(got.iter().all(|(_, a)| a == &want), "{got:?}");

        // A push that deletes a branch asks first; a no is a clean refusal.
        let z = "0".repeat(40);
        let a = "a".repeat(40);
        let mut body = broker::tests_pkt(
            format!("{a} {z} refs/heads/old\0report-status side-band-64k\n").as_bytes(),
        );
        body.extend_from_slice(b"0000");
        let url = format!("{base}github/o/r.git/git-receive-pack");
        let t = token.clone();
        let post = tokio::spawn(async move {
            reqwest::Client::new()
                .post(url)
                .bearer_auth(t)
                .body(body)
                .send()
                .await
                .unwrap()
                .bytes()
                .await
                .unwrap()
        });
        let id = loop {
            if let EngineEvent::ApprovalRequested { approval } = rx.recv().await.unwrap() {
                assert_eq!(approval.summary, "Push to o/r: delete old");
                break approval.id;
            }
        };
        hub.resolve_approval(&id, Decision::Deny("no".into()));
        let r = String::from_utf8_lossy(&post.await.unwrap()).to_string();
        assert!(
            r.contains("ng refs/heads/old the person didn't allow this push"),
            "{r}"
        );
        assert!(
            !seen
                .lock()
                .unwrap()
                .iter()
                .any(|(u, _)| u.contains("receive-pack"))
        );

        // Wrong token or an unknown chat: nothing goes out.
        let r = reqwest::Client::new()
            .get(format!("{base}github/o/r.git/info/refs"))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 401);
        let _ = std::fs::remove_dir_all(&root);
    }

    async fn server_call(url: &str, session: &str, body: Value) -> Value {
        reqwest::Client::new()
            .post(url)
            .bearer_auth("secret")
            .header("x-tab-session", session)
            .json(&body)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn agents_draft_workflows_but_never_turn_them_on() {
        let store = Arc::new(Store::in_memory().unwrap());
        let (events, mut rx) = tokio::sync::broadcast::channel(64);
        let hub = AgentHub::new(
            store.clone(),
            events.clone(),
            AgentConfig::default(),
            std::env::temp_dir(),
        );
        let server = McpServer {
            store: store.clone(),
            hub,
            engine: Arc::new(NoEngine(store.clone())),
            events,
            token: "t".into(),
        };
        let listed = server
            .call(
                None,
                "tab_list_actions",
                json!({ "connector_id": "github" }),
            )
            .await
            .unwrap();
        assert!(
            listed["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("pr.request_reviewers")
        );

        let draft = json!({
            "name": "Snooze bots",
            "enabled": true,
            "allow_destructive": true,
            "trigger": { "type": "item", "changes": ["ingested"], "sources": ["github"], "when": [{ "field": "item.actor", "op": "matches", "value": "*[bot]" }] },
            "steps": [{ "id": "later", "type": "action", "connector": "tab", "action": "item.snooze", "input": { "minutes": 600 } }]
        });
        let saved = server
            .call(None, "tab_save_workflow", json!({ "workflow": draft }))
            .await
            .unwrap();
        assert!(
            saved["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("turned off")
        );
        let w = &store.workflows().unwrap()[0];
        assert!(
            !w.enabled && !w.allow_destructive,
            "only the person turns these on"
        );
        assert_eq!(w.created_by, "agent");
        // The item input was filled from the action's default.
        let crate::workflow::StepKind::Action { input, .. } = &w.steps[0].kind else {
            panic!()
        };
        assert_eq!(input["item_id"], "{{item.id}}");
        assert!(matches!(
            rx.try_recv(),
            Ok(EngineEvent::WorkflowsChanged { .. })
        ));

        // A task on every new item would feed itself: refused with the reason.
        let looping = json!({
            "name": "Echo",
            "trigger": { "type": "item", "changes": ["ingested"] },
            "steps": [{ "id": "t", "type": "action", "connector": "tab", "action": "task.create", "input": { "title": "again" } }]
        });
        let err = server
            .call(None, "tab_save_workflow", json!({ "workflow": looping }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("could loop"), "{err}");
        assert_eq!(store.workflows().unwrap().len(), 1);
    }
}
