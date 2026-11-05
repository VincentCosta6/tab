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
//! * `tab_project`, `tab_project_note`: a project's overview, and durable
//!   notes agents leave for the next chat.
//! * `tab_list_connections`, `tab_find_email_server`, `tab_configure_connection`,
//!   `tab_ask_for_secret`, `tab_test_connection`: the setup assistant. Agents
//!   fill in everything but secrets; secrets go through a card in the chat
//!   that the person types into, so the agent never sees them.
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
use crate::model::{
    ConnectorInfo, ContextSection, Entity, ItemState, NewContext, NewTask, Preview, Project,
    ProjectNote, QueueItem, SettingKind,
};
use crate::store::Store;

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
        .with_state(server);
    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::error!(error = %e, "MCP server stopped");
        }
    });
    Ok(url)
}

async fn handle(State(s): State<Arc<McpServer>>, headers: HeaderMap, body: String) -> Response {
    let authed = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|t| !s.token.is_empty() && t == s.token);
    if !authed {
        return (StatusCode::UNAUTHORIZED, "missing or wrong bearer token").into_response();
    }
    let session = headers
        .get("x-tab-session")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
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

pub fn tools() -> Value {
    json!([
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
                "item_id": { "type": "string", "description": "Task to file it on. Defaults to this session's task." }
            }, "required": ["title"] }
        },
        {
            "name": "tab_attach_context",
            "description": "Attach a queue item or link directly to the current task's Context. Use for things that are directly part of the work: the PR you opened for it, the ticket it implements, the thread or email you're answering, or anything the person names. For things that only might be relevant, use tab_add_found instead.",
            "inputSchema": { "type": "object", "properties": {
                "ref_item_id": { "type": "string" },
                "url": { "type": "string" },
                "title": { "type": "string" },
                "item_id": { "type": "string", "description": "Task to attach to. Defaults to this session's task." }
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
            "description": "A project's overview: its notes, the repos, people and tickets its work is about, and its recent items. Defaults to this chat's project; with no project, lists them all.",
            "inputSchema": { "type": "object", "properties": {
                "project": { "type": "string", "description": "Project name or id." },
                "within_days": { "type": "integer", "minimum": 1, "description": "Only count activity from the last N days, so what is connected now shows first. Leave out for all time." }
            } }
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
            "tools/list" => Ok(json!({ "tools": tools() })),
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
                }))?))
            }
            "tab_add_found" | "tab_attach_context" => {
                let item_id = self.session_item(session, &args)?;
                let ref_item_id = s("ref_item_id");
                let ref_item = match &ref_item_id {
                    Some(r) => Some(
                        self.store
                            .get(r)?
                            .ok_or_else(|| anyhow::anyhow!("no item {r}"))?,
                    ),
                    None => None,
                };
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
                let _ = self.events.send(EngineEvent::ContextChanged { item_id });
                Ok(text(format!(
                    "Added \"{}\" under {}.",
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
                    "notes": self.store.project_notes(&p.id)?.iter().map(note_json).collect::<Vec<_>>(),
                    "about": self.store.project_entities(&p.id, since, 15)?.iter()
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
            _ => anyhow::bail!("unknown tool {name}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AgentConfig;
    use crate::model::{AgentKind, IncomingEvent};

    struct NoEngine(Arc<Store>);

    #[async_trait]
    impl EngineAccess for NoEngine {
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
        assert_eq!(list["result"]["tools"].as_array().unwrap().len(), 16);

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
        out(server
            .call(
                Some(&session.id),
                "tab_project_note",
                json!({"topic":"deploys","delete":true}),
            )
            .await);
        assert!(store.project_notes(&p.id).unwrap().is_empty());
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
}
