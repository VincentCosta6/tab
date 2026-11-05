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
use crate::engine::EngineEvent;
use crate::model::{ContextSection, ItemState, NewContext, NewTask, Preview, QueueItem};
use crate::store::Store;

/// What the MCP server needs from the running engine.
#[async_trait]
pub trait EngineAccess: Send + Sync + 'static {
    async fn create_task(&self, task: NewTask) -> anyhow::Result<QueueItem>;
    async fn preview(&self, item_id: &str) -> anyhow::Result<Preview>;
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

impl McpServer {
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
                "instructions": "tab is the person's work queue. Search it, read items, file what you find under Found, attach context and create tasks."
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
        assert_eq!(list["result"]["tools"].as_array().unwrap().len(), 6);

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
