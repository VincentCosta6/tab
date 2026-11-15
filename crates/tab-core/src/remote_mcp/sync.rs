//! Filling the queue from MCP connections ("Fills your queue").
//!
//! MCP has no standard way to ask "what's new for me", so each catalog
//! server tab knows can have a [`Recipe`]: which read tools to call and how
//! their answers map to queue items. Recipes are data, written as manifests
//! ([`super::manifest`]). A connection without one gives agents tools only.
//!
//! [`McpConnector`] runs a recipe like any other connector: it calls the
//! server as tab itself (with the connection's sign-in, through
//! [`RemoteMcp::call_tool`]), under the gateway's budgets and audit log, and
//! emits items with the server's id for each thing as `external_id`. Things
//! that finished upstream (a Linear issue done, a Sentry issue resolved) are
//! marked done in the queue.
//!
//! Items use the connection's id as their source (`linear`), so turning the
//! queue off for it clears them like any other connection.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;

use super::{RemoteMcp, RemoteMcpConfig};
use crate::TabConfig;
use crate::connector::{Connector, ConnectorContext};
use crate::gateway::{Call, Gateway, GatewayError};
use crate::model::{ConnectorStatus, IncomingEvent, KindInfo, QueueItem, RateLimitSpec};

/// One thing a recipe found.
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    /// `source` is filled in by the connector.
    pub event: IncomingEvent,
    /// Finished upstream (done, cancelled, resolved): marked done in the
    /// queue rather than added.
    pub resolved: bool,
}

/// One option for what a connection brings into the queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct QueueChoice {
    /// What's saved in [`RemoteMcpConfig::queue_query`].
    pub value: &'static str,
    pub label: &'static str,
}

/// How tab reads one kind of server. See the module docs.
#[async_trait]
pub trait Recipe: Send + Sync {
    /// The kinds of work it brings, for the queue's labels and icons.
    fn kinds(&self) -> Vec<KindInfo>;

    /// What comes into the queue and when it's marked done, for the
    /// connection's "Fills your queue" section.
    fn description(&self) -> &'static str;

    /// How often to look.
    fn interval(&self) -> Duration {
        Duration::from_secs(5 * 60)
    }

    /// Whether later looks list only what changed since the last one
    /// (rather than everything open).
    fn incremental(&self) -> bool {
        false
    }

    /// For recipes that watch something the person picks: what to show
    /// in the empty field.
    fn query_hint(&self) -> Option<&'static str> {
        None
    }

    /// For recipes where the person picks what comes in from a short list:
    /// the choices, kept in [`RemoteMcpConfig::queue_query`]. The first is
    /// the default.
    fn queue_choices(&self) -> &'static [QueueChoice] {
        &[]
    }

    /// Looks for work. `since` is when the last look started (`None` on
    /// the first look, which should list everything open for the person).
    async fn poll(
        &self,
        s: &Session<'_>,
        since: Option<DateTime<Utc>>,
    ) -> anyhow::Result<Vec<Found>>;

    /// Whether the thing behind an item (as last emitted) finished
    /// upstream. `None` when the recipe can't tell.
    async fn check(&self, _s: &Session<'_>, _item: &IncomingEvent) -> anyhow::Result<Option<bool>> {
        Ok(None)
    }
}

/// The recipe for a catalog entry, if its manifest fills the queue.
pub fn recipe(catalog: &str) -> Option<&'static dyn Recipe> {
    super::manifest::recipe(catalog)
}

/// The recipe a connection would sync with: it came from a catalog entry
/// with one, and its id doesn't belong to a built-in connector.
pub fn recipe_for(id: &str, c: &RemoteMcpConfig) -> Option<&'static dyn Recipe> {
    if crate::connectors::known(id) {
        return None;
    }
    recipe(c.catalog.as_deref()?)
}

/// Whether items with this source come from an MCP connection that fills
/// the queue right now (so the app keeps them).
pub fn syncs(cfg: &TabConfig, source: &str) -> bool {
    cfg.mcp_servers
        .get(source)
        .is_some_and(|c| c.enabled && c.queue != Some(false) && recipe_for(source, c).is_some())
}

/// Builds a connector for every MCP connection that fills the queue.
pub fn build(
    cfg: &TabConfig,
    remote: &Arc<RemoteMcp>,
    gateway: &Arc<Gateway>,
) -> Vec<Arc<dyn Connector>> {
    cfg.mcp_servers
        .iter()
        .filter(|(id, _)| syncs(cfg, id))
        .filter_map(|(id, c)| {
            let recipe = recipe_for(id, c)?;
            gateway.declare_limits(&[limits(id)]);
            Some(Arc::new(McpConnector {
                id: id.clone(),
                recipe,
                remote: remote.clone(),
                gateway: gateway.clone(),
                query: c.queue_query.clone().filter(|q| !q.trim().is_empty()),
                since: Mutex::new(None),
                open: Mutex::new(HashMap::new()),
            }) as Arc<dyn Connector>)
        })
        .collect()
}

/// tab's own budget for one server: generous for a few polls an hour, low
/// enough that a bug can't hammer it.
fn limits(id: &str) -> RateLimitSpec {
    RateLimitSpec::new(&provider(id), "default", 120, 3600, 10)
}

fn provider(id: &str) -> String {
    format!("mcp:{id}")
}

/// What a recipe calls the server through.
pub struct Session<'a> {
    id: &'a str,
    remote: &'a RemoteMcp,
    gateway: &'a Gateway,
    /// The person's pick, for recipes that watch something they choose.
    pub query: Option<&'a str>,
    tools: Vec<String>,
    /// The tools the server marks read-only.
    read_only: Vec<String>,
}

impl Session<'_> {
    /// Whether the server has this tool.
    pub fn has(&self, tool: &str) -> bool {
        self.tools.iter().any(|t| t == tool)
    }

    /// Whether the server marks this tool read-only.
    pub fn read_only(&self, tool: &str) -> bool {
        self.read_only.iter().any(|t| t == tool)
    }

    /// The first of `names` the server has.
    pub fn pick<'n>(&self, names: &[&'n str]) -> Option<&'n str> {
        names.iter().copied().find(|n| self.has(n))
    }

    /// Calls a tool and returns what it said: its structured content, or
    /// its text parsed as JSON, or the text itself.
    pub async fn call(&self, tool: &str, args: Value) -> anyhow::Result<Value> {
        let call = Call::post(
            &provider(self.id),
            self.id,
            "queue sync",
            format!("mcp://{}/{tool}", self.id),
        );
        let result = self
            .gateway
            .run(call, self.remote.call_tool(self.id, tool, args))
            .await?;
        Ok(payload(&result))
    }
}

/// The text parts of a tool result, joined.
pub fn text_of(result: &Value) -> String {
    result["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["type"] == "text")
        .filter_map(|c| c["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// What a tool result holds: `structuredContent` when the server sends it,
/// otherwise its text, as JSON when it parses.
pub fn payload(result: &Value) -> Value {
    if let Some(s) = result.get("structuredContent").filter(|s| !s.is_null()) {
        return s.clone();
    }
    let text = text_of(result);
    serde_json::from_str(text.trim()).unwrap_or(Value::String(text))
}

/// The list in a payload: the payload itself, or the first array under
/// one of `keys` (servers wrap lists differently).
pub(crate) fn list<'a>(v: &'a Value, keys: &[&str]) -> Vec<&'a Value> {
    if let Some(a) = v.as_array() {
        return a.iter().collect();
    }
    for k in keys {
        if let Some(a) = v[*k].as_array() {
            return a.iter().collect();
        }
    }
    Vec::new()
}

/// A string field, or the `name` of an object field (`status: {name}`).
pub(crate) fn name_of(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Object(o) => o
            .get("name")
            .or_else(|| o.get("displayName"))
            .or_else(|| o.get("title"))
            .and_then(Value::as_str)
            .map(str::to_string),
        _ => None,
    }
}

pub(crate) fn time_of(v: &Value) -> Option<DateTime<Utc>> {
    v.as_str()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&Utc))
}

pub(crate) fn clip(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

/// A connection's queue sync, run by the engine like any connector.
pub struct McpConnector {
    id: String,
    recipe: &'static dyn Recipe,
    remote: Arc<RemoteMcp>,
    gateway: Arc<Gateway>,
    query: Option<String>,
    /// When the last good look started.
    since: Mutex<Option<DateTime<Utc>>>,
    /// What the last look found open, by external id.
    open: Mutex<HashMap<String, IncomingEvent>>,
}

/// Things that dropped out of a list are checked one by one, at most this
/// many per look.
const CHECKS_PER_LOOK: usize = 20;

/// How long to wait before looking again while it isn't signed in.
const NOT_READY_EVERY: Duration = Duration::from_secs(20);

impl McpConnector {
    async fn session(&self) -> anyhow::Result<Session<'_>> {
        let tools = self.remote.tool_names(&self.id).await?;
        let read_only = self
            .remote
            .tool_views(&self.id)
            .into_iter()
            .filter(|t| t.read_only)
            .map(|t| t.name)
            .collect();
        Ok(Session {
            id: &self.id,
            remote: &self.remote,
            gateway: &self.gateway,
            query: self.query.as_deref(),
            tools,
            read_only,
        })
    }

    /// One look: emits what's open, resolves what finished.
    async fn look(&self, ctx: &ConnectorContext) -> anyhow::Result<usize> {
        let started = Utc::now();
        let since = *self.since.lock().unwrap();
        let s = self.session().await?;
        let found = self.recipe.poll(&s, since).await?;
        let mut seen = HashSet::new();
        let mut open = HashMap::new();
        for mut f in found {
            if !seen.insert(f.event.external_id.clone()) {
                continue;
            }
            f.event.source = self.id.clone();
            if f.resolved {
                ctx.resolve(&f.event.external_id).await;
            } else {
                open.insert(f.event.external_id.clone(), f.event.clone());
                ctx.emit(f.event).await?;
            }
        }
        let before = std::mem::take(&mut *self.open.lock().unwrap());
        if self.recipe.incremental() {
            // Lists only what changed: what's missing just didn't change.
            let mut all = before;
            for id in &seen {
                all.remove(id);
            }
            all.extend(open);
            *self.open.lock().unwrap() = all;
        } else {
            // Lists everything open: what dropped out may have finished
            // (or just moved to someone else, which leaves it in the queue).
            let gone: Vec<IncomingEvent> = before
                .into_values()
                .filter(|e| !seen.contains(&e.external_id))
                .take(CHECKS_PER_LOOK)
                .collect();
            for e in gone {
                if self.recipe.check(&s, &e).await? == Some(true) {
                    ctx.resolve(&e.external_id).await;
                }
            }
            *self.open.lock().unwrap() = open;
        }
        *self.since.lock().unwrap() = Some(started);
        let n = self.open.lock().unwrap().len();
        Ok(n)
    }
}

#[async_trait]
impl Connector for McpConnector {
    fn id(&self) -> &str {
        &self.id
    }

    async fn run(&self, ctx: ConnectorContext) -> anyhow::Result<()> {
        let mut signed_in = self.remote.subscribe();
        loop {
            let wait = if !self.remote.is_ready(&self.id) {
                NOT_READY_EVERY
            } else {
                match crate::ratelimit::background(self.look(&ctx)).await {
                    Ok(n) => {
                        self.remote.set_synced(&self.id, |q| {
                            q.last_sync = Some(Utc::now());
                            q.error = None;
                            q.found = Some(n);
                        });
                        ctx.synced().await;
                        self.recipe.interval()
                    }
                    // Held back to leave room: try again soon, quietly.
                    Err(e)
                        if matches!(
                            e.downcast_ref::<GatewayError>(),
                            Some(GatewayError::Deferred { .. })
                        ) =>
                    {
                        Duration::from_secs(60)
                    }
                    Err(e) => {
                        tracing::warn!(connection = %self.id, error = %e, "queue sync failed");
                        let msg = e.to_string();
                        self.remote
                            .set_synced(&self.id, |q| q.error = Some(msg.clone()));
                        ctx.set_status(ConnectorStatus::Error { message: msg })
                            .await;
                        self.recipe.interval()
                    }
                }
            };
            tokio::select! {
                _ = tokio::time::sleep(wait) => {}
                // Signing in looks right away.
                Ok(e) = signed_in.recv() => {
                    if e.id != self.id || !e.ok {
                        continue;
                    }
                }
                _ = ctx.cancelled() => return Ok(()),
            }
        }
    }

    async fn is_resolved(&self, item: &QueueItem) -> anyhow::Result<bool> {
        if !self.remote.is_ready(&self.id) {
            return Ok(false);
        }
        let s = self.session().await?;
        Ok(
            crate::ratelimit::background(self.recipe.check(&s, &item.event))
                .await?
                .unwrap_or(false),
        )
    }

    fn preview_ttl(&self) -> Duration {
        self.recipe.interval()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn payload_prefers_structured_then_json_then_text() {
        let structured = json!({ "structuredContent": { "a": 1 }, "content": [{ "type": "text", "text": "x" }] });
        assert_eq!(payload(&structured), json!({ "a": 1 }));
        let text_json = json!({ "content": [{ "type": "text", "text": " [1, 2] " }] });
        assert_eq!(payload(&text_json), json!([1, 2]));
        let text = json!({ "content": [{ "type": "text", "text": "# Hi" }, { "type": "text", "text": "there" }] });
        assert_eq!(payload(&text), json!("# Hi\nthere"));
    }

    /// A Sentry-like server: two organizations' worth of tools, one issue
    /// that resolves between looks.
    #[tokio::test]
    async fn polls_a_server_and_resolves_what_finished() {
        use axum::{Router, extract::State, routing::post};
        use std::sync::atomic::{AtomicUsize, Ordering};

        let looks = Arc::new(AtomicUsize::new(0));
        async fn mcp(
            State(looks): State<Arc<AtomicUsize>>,
            body: String,
        ) -> axum::response::Response {
            use axum::response::IntoResponse;
            let req: Value = serde_json::from_str(&body).unwrap();
            let id = req["id"].clone();
            let result = match req["method"].as_str().unwrap_or_default() {
                "initialize" => {
                    json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "serverInfo": { "name": "S" } })
                }
                "tools/list" => json!({ "tools": [
                    { "name": "find_organizations" }, { "name": "search_issues" }, { "name": "get_issue_details" } ] }),
                "tools/call" => {
                    let args = &req["params"]["arguments"];
                    let out = match req["params"]["name"].as_str().unwrap() {
                        "find_organizations" => {
                            json!({ "organizations": [{ "slug": "acme", "regionUrl": "https://us.sentry.io" }] })
                        }
                        "search_issues" if args["query"] == "is:unresolved is:unassigned" => {
                            // Unassigned, by default: one new, one also mine
                            // (counted once).
                            json!({ "issues": [
                                { "shortId": "WEB-1", "title": "Boom", "status": "unresolved" },
                                { "shortId": "WEB-3", "title": "Nobody's", "status": "unresolved" },
                            ] })
                        }
                        "search_issues" => {
                            assert_eq!(args["query"], "is:unresolved assigned:me");
                            assert_eq!(args["regionUrl"], "https://us.sentry.io");
                            let n = looks.fetch_add(1, Ordering::SeqCst);
                            let mut issues = vec![
                                json!({ "shortId": "WEB-1", "title": "Boom", "status": "unresolved" }),
                            ];
                            if n == 0 {
                                issues.push(json!({ "shortId": "WEB-2", "title": "Gone soon", "status": "unresolved" }));
                            }
                            json!({ "issues": issues })
                        }
                        "get_issue_details" => {
                            assert_eq!(args["issueId"], "WEB-2");
                            json!({ "shortId": "WEB-2", "status": "resolved" })
                        }
                        other => panic!("unexpected tool {other}"),
                    };
                    json!({ "content": [{ "type": "text", "text": out.to_string() }], "structuredContent": out })
                }
                _ => return axum::http::StatusCode::ACCEPTED.into_response(),
            };
            (
                [("content-type", "application/json")],
                json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string(),
            )
                .into_response()
        }
        let app = Router::new()
            .route("/mcp", post(mcp))
            .with_state(looks.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let remote =
            RemoteMcp::new(std::env::temp_dir().join(format!("tab-sync-{}", uuid::Uuid::new_v4())));
        let mut cfg = TabConfig::default();
        cfg.mcp_servers.insert(
            "sentry".into(),
            RemoteMcpConfig {
                name: "Sentry".into(),
                url: format!("http://{addr}/mcp"),
                enabled: true,
                catalog: Some("sentry".into()),
                ..Default::default()
            },
        );
        remote.set_servers(cfg.mcp_servers.clone());
        let store = Arc::new(crate::store::Store::in_memory().unwrap());
        let gateway = Arc::new(Gateway::new(store.clone()));
        assert_eq!(build(&cfg, &remote, &gateway).len(), 1);
        let m = Arc::new(McpConnector {
            id: "sentry".into(),
            recipe: recipe("sentry").unwrap(),
            remote: remote.clone(),
            gateway: gateway.clone(),
            query: None,
            since: Mutex::new(None),
            open: Mutex::new(HashMap::new()),
        });

        // The first look, through the run loop: both issues arrive, and
        // the connection says how it went.
        let (ctx, mut rx, cancel) = crate::connector::test_context("sentry");
        let run = m.clone();
        let task = tokio::spawn(async move { run.run(ctx).await });
        let mut lines = Vec::new();
        while lines.len() < 4 {
            lines.push(rx.recv().await.unwrap());
        }
        cancel.cancel();
        task.await.unwrap().unwrap();
        assert!(
            lines[0].starts_with("event sentry.issue | acme/WEB-1 | WEB-1 Boom"),
            "{lines:?}"
        );
        assert!(
            lines[1].starts_with("event sentry.issue | acme/WEB-2"),
            "{lines:?}"
        );
        assert!(
            lines[2].starts_with("event sentry.issue | acme/WEB-3 | WEB-3 Nobody's"),
            "{lines:?}"
        );
        assert!(lines[3].starts_with("status sentry: Running"), "{lines:?}");
        let q = remote.infos()[0].queue_sync.clone().unwrap();
        assert!(q.on);
        assert_eq!(q.found, Some(3));
        assert!(q.last_sync.is_some() && q.error.is_none());

        // The second look: WEB-2 dropped out of the list, so tab asks
        // about it, and it resolved.
        let (ctx, mut rx, _c) = crate::connector::test_context("sentry");
        assert_eq!(m.look(&ctx).await.unwrap(), 2);
        drop(ctx);
        let mut rest = Vec::new();
        while let Some(l) = rx.recv().await {
            rest.push(l);
        }
        assert_eq!(rest.len(), 3, "{rest:?}");
        assert!(rest[0].starts_with("event sentry.issue | acme/WEB-1"));
        assert!(rest[1].starts_with("event sentry.issue | acme/WEB-3"));
        assert_eq!(rest[2], "resolved acme/WEB-2");

        // Every tool call went through the gateway's log.
        let audit = store
            .audit_list(&crate::store::AuditQuery::default())
            .unwrap();
        assert!(
            audit
                .iter()
                .any(|a| a.connector == "sentry" && a.url == "mcp://sentry/search_issues")
        );
    }

    #[test]
    fn only_catalog_connections_with_recipes_sync() {
        let mut cfg = TabConfig::default();
        let c = |catalog: Option<&str>| RemoteMcpConfig {
            name: "x".into(),
            url: "https://x".into(),
            enabled: true,
            catalog: catalog.map(str::to_string),
            ..Default::default()
        };
        cfg.mcp_servers.insert("linear".into(), c(Some("linear")));
        cfg.mcp_servers.insert("stripe".into(), c(Some("stripe")));
        cfg.mcp_servers.insert("custom".into(), c(None));
        // A connection can't take a built-in connector's place.
        cfg.mcp_servers.insert("github".into(), c(Some("linear")));
        let mut off = c(Some("sentry"));
        off.queue = Some(false);
        cfg.mcp_servers.insert("sentry".into(), off);
        assert!(syncs(&cfg, "linear"));
        assert!(!syncs(&cfg, "stripe"));
        assert!(!syncs(&cfg, "custom"));
        assert!(!syncs(&cfg, "github"));
        assert!(!syncs(&cfg, "sentry"));
        assert!(!syncs(&cfg, "nope"));
    }
}
