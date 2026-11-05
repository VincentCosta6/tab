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
    ConnectorInfo, ContextSection, ItemState, NewContext, NewTask, Preview, QueueItem, SettingKind,
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
        "state": i.state,
        "urgency": i.ranking.as_ref().map(|r| r.urgency),
        "score": i.ranking.as_ref().map(|r| r.score),
    })
}

pub fn tools() -> Value {
    json!([
        {
            "name": "tab_search",
            "description": "Search the person's work queue (Slack, GitHub, email, meetings, Jira, tab tasks) by substring. Returns items with ids you can attach or read.",
            "inputSchema": { "type": "object", "properties": {
                "query": { "type": "string", "description": "Words to look for; at least 3 characters works best." },
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
            "description": "File something relevant you found (a PR, ticket, doc, thread, file) under Found on the current task, so the person can promote it into context.",
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
            "description": "Attach a queue item or link as context on the current task. Use when the person asks you to work on a specific ticket, PR or thread.",
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
                "url": { "type": "string" }
            }, "required": ["title"] }
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
                "instructions": "tab is the person's work queue. Search it, read items, file what you find under Found, attach context and create tasks, and set up connections (email, GitHub, ...) for the person."
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
                let items = self.store.search(&q, &states, limit)?;
                Ok(text(serde_json::to_string_pretty(
                    &items.iter().map(brief).collect::<Vec<_>>(),
                )?))
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
                let item = self
                    .engine
                    .create_task(NewTask {
                        title: s("title").ok_or_else(|| anyhow::anyhow!("title is required"))?,
                        body: s("body"),
                        priority: s("priority"),
                        due_at,
                        url: s("url"),
                        created_by: "agent".into(),
                    })
                    .await?;
                Ok(text(format!(
                    "Created task {} ({})",
                    item.id, item.event.kind
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
        assert_eq!(list["result"]["tools"].as_array().unwrap().len(), 11);

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
