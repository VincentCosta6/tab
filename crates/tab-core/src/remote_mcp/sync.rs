//! Filling the queue from MCP connections ("Fills your queue").
//!
//! MCP has no standard way to ask "what's new for me", so each catalog
//! server tab knows gets a small [`Recipe`]: which read tool to call and how
//! its answer maps to queue items. A connection without a recipe gives
//! agents tools only.
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
use serde_json::{Value, json};

use super::{RemoteMcp, RemoteMcpConfig};
use crate::TabConfig;
use crate::connector::{Connector, ConnectorContext};
use crate::gateway::{Call, Gateway, GatewayError};
use crate::model::{
    Block, ConnectorStatus, EntityRef, Field, IncomingEvent, KindInfo, Link, QueueItem,
    RateLimitSpec, Tone,
};

/// One thing a recipe found.
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    /// `source` is filled in by the connector.
    pub event: IncomingEvent,
    /// Finished upstream (done, cancelled, resolved): marked done in the
    /// queue rather than added.
    pub resolved: bool,
}

/// How tab reads one kind of server. See the module docs.
#[async_trait]
pub trait Recipe: Send + Sync {
    /// The kinds of work it brings, for the queue's labels and icons.
    fn kinds(&self) -> Vec<KindInfo>;

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

/// The recipe for a catalog entry, if tab has one.
pub fn recipe(catalog: &str) -> Option<&'static dyn Recipe> {
    match catalog {
        "linear" => Some(&Linear),
        "sentry" => Some(&Sentry),
        "notion" => Some(&Notion),
        _ => None,
    }
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
}

impl Session<'_> {
    /// Whether the server has this tool.
    pub fn has(&self, tool: &str) -> bool {
        self.tools.iter().any(|t| t == tool)
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
fn list<'a>(v: &'a Value, keys: &[&str]) -> Vec<&'a Value> {
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
fn name_of(v: &Value) -> Option<String> {
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

fn time_of(v: &Value) -> Option<DateTime<Utc>> {
    v.as_str()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&Utc))
}

fn clip(s: &str, max: usize) -> String {
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
        Ok(Session {
            id: &self.id,
            remote: &self.remote,
            gateway: &self.gateway,
            query: self.query.as_deref(),
            tools: self.remote.tool_names(&self.id).await?,
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

// ---------------------------------------------------------------------------
// Linear
// ---------------------------------------------------------------------------

/// Issues assigned to you.
struct Linear;

impl Linear {
    fn found(issue: &Value) -> Option<Found> {
        // Newer servers put the key ("ENG-12") in `id` and the UUID in
        // `uuid`; older ones have `id` (UUID) and `identifier`.
        let id = issue["uuid"].as_str().or_else(|| issue["id"].as_str())?;
        let key = issue["identifier"]
            .as_str()
            .or_else(|| issue["id"].as_str().filter(|i| looks_like_short_id(i)));
        let title = issue["title"].as_str().unwrap_or("Untitled issue");
        // `status` is a name ("In Progress") or an object with a `type`
        // (`completed`, `canceled`, …), depending on the server's version.
        let status = name_of(&issue["status"]).or_else(|| name_of(&issue["state"]));
        let status_type = issue["status"]["type"]
            .as_str()
            .or_else(|| issue["state"]["type"].as_str())
            .or_else(|| issue["statusType"].as_str());
        let set = |k: &str| issue.get(k).is_some_and(|v| !v.is_null());
        let resolved = set("completedAt")
            || set("canceledAt")
            || set("archivedAt")
            || matches!(
                status_type,
                Some("completed" | "canceled" | "cancelled" | "duplicate")
            )
            || status.as_deref().is_some_and(|s| {
                matches!(
                    s.to_ascii_lowercase().as_str(),
                    "done" | "canceled" | "cancelled" | "duplicate" | "completed"
                )
            });
        let team = name_of(&issue["team"]);
        let project = name_of(&issue["project"]);
        let priority = name_of(&issue["priority"])
            .or_else(|| issue["priorityLabel"].as_str().map(str::to_string));
        let mut fields = Vec::new();
        if let Some(s) = &status {
            fields.push(Field::new("Status", s.clone()));
        }
        if let Some(p) = priority.as_ref().filter(|p| p.as_str() != "No priority") {
            let tone = match p.as_str() {
                "Urgent" => Tone::Danger,
                "High" => Tone::Warning,
                _ => Tone::Neutral,
            };
            fields.push(Field::new("Priority", p.clone()).tone(tone));
        }
        if let Some(p) = &project {
            fields.push(Field::new("Project", p.clone()));
        }
        if let Some(d) = issue["dueDate"].as_str() {
            fields.push(Field::new("Due", d));
        }
        let description = issue["description"]
            .as_str()
            .filter(|d| !d.trim().is_empty());
        let mut blocks = Vec::new();
        if !fields.is_empty() {
            blocks.push(Block::Badges { items: fields });
        }
        if let Some(d) = description {
            blocks.push(Block::Text {
                text: clip(d, 1500),
                markdown: true,
            });
        }
        let url = issue["url"].as_str().map(str::to_string);
        if let Some(u) = &url {
            blocks.push(Block::Links {
                items: vec![Link {
                    label: "Open in Linear".into(),
                    url: u.clone(),
                }],
            });
        }
        let mut entities = Vec::new();
        if let Some(t) = &team {
            entities.push(EntityRef {
                kind: "team".into(),
                key: format!("linear:team:{t}"),
                label: t.clone(),
                rel: "in".into(),
            });
        }
        if let Some(p) = &project {
            entities.push(EntityRef {
                kind: "project".into(),
                key: format!("linear:project:{p}"),
                label: p.clone(),
                rel: "in".into(),
            });
        }
        let title = match key {
            Some(k) => format!("{k} {title}"),
            None => title.to_string(),
        };
        Some(Found {
            resolved,
            event: IncomingEvent {
                source: String::new(),
                external_id: id.to_string(),
                kind: "linear.issue".into(),
                title,
                body: description.map(|d| clip(d, 400)),
                url,
                actor: name_of(&issue["createdBy"]).or_else(|| name_of(&issue["creator"])),
                context: team.clone().or(project.clone()),
                occurred_at: time_of(&issue["updatedAt"])
                    .or_else(|| time_of(&issue["createdAt"]))
                    .unwrap_or_else(Utc::now),
                blocks,
                entities,
                metadata: json!({
                    "identifier": key,
                    "status": status,
                    "priority": priority,
                }),
            },
        })
    }
}

#[async_trait]
impl Recipe for Linear {
    fn kinds(&self) -> Vec<KindInfo> {
        vec![KindInfo::new(
            "linear.issue",
            "Linear issue",
            "linear",
            Tone::Accent,
        )]
    }

    fn incremental(&self) -> bool {
        true
    }

    async fn poll(
        &self,
        s: &Session<'_>,
        since: Option<DateTime<Utc>>,
    ) -> anyhow::Result<Vec<Found>> {
        let tool = s
            .pick(&["list_issues", "list_my_issues"])
            .ok_or_else(|| anyhow::anyhow!("Linear has no tool to list issues"))?;
        let mut args = json!({
            "assignee": "me", "limit": 100, "orderBy": "updatedAt", "includeArchived": false,
        });
        // Later looks: everything that changed since, finished or not, so
        // finished ones leave the queue. The first look: what's open now.
        if let Some(t) = since {
            args["updatedAt"] = json!((t - chrono::Duration::minutes(5)).to_rfc3339());
        }
        let mut found = Vec::new();
        for _ in 0..3 {
            let v = s.call(tool, args.clone()).await?;
            found.extend(
                list(&v, &["issues", "nodes", "results", "data"])
                    .into_iter()
                    .filter_map(Linear::found),
            );
            match v["cursor"].as_str().or_else(|| v["nextCursor"].as_str()) {
                Some(c) if v["hasNextPage"] == true => args["cursor"] = json!(c),
                _ => break,
            }
        }
        Ok(match since {
            Some(_) => found,
            None => found.into_iter().filter(|f| !f.resolved).collect(),
        })
    }

    async fn check(&self, s: &Session<'_>, item: &IncomingEvent) -> anyhow::Result<Option<bool>> {
        if !s.has("get_issue") {
            return Ok(None);
        }
        let id = item.metadata["identifier"]
            .as_str()
            .unwrap_or(&item.external_id);
        let v = s.call("get_issue", json!({ "id": id })).await?;
        let issue = if v["issue"].is_object() {
            &v["issue"]
        } else {
            &v
        };
        Ok(Linear::found(issue).map(|f| f.resolved))
    }
}

// ---------------------------------------------------------------------------
// Sentry
// ---------------------------------------------------------------------------

/// Unresolved issues assigned to you, in every organization you're in.
struct Sentry;

/// The query Sentry runs for the queue.
const SENTRY_QUERY: &str = "is:unresolved assigned:me";

impl Sentry {
    /// Organization slugs from `find_organizations`: JSON when the server
    /// sends it, otherwise its markdown (`## **slug**` or `- slug`).
    fn orgs(v: &Value) -> Vec<(String, Option<String>)> {
        let from_json: Vec<(String, Option<String>)> =
            list(v, &["organizations", "results", "data"])
                .into_iter()
                .filter_map(|o| {
                    let slug = o["slug"].as_str()?.to_string();
                    Some((
                        slug,
                        o["regionUrl"]
                            .as_str()
                            .filter(|r| !r.is_empty())
                            .map(str::to_string),
                    ))
                })
                .collect();
        if !from_json.is_empty() {
            return from_json;
        }
        let Some(text) = v.as_str() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for line in text.lines() {
            let l = line.trim();
            let slug = if let Some(rest) = l.strip_prefix("## ") {
                Some(rest.trim().trim_matches('*').trim())
            } else {
                l.strip_prefix("- **Slug**:")
                    .or_else(|| l.strip_prefix("**Slug**:"))
                    .map(|rest| rest.trim().trim_matches('`'))
            };
            if let Some(s) = slug
                && !s.is_empty()
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
                && !out.iter().any(|(o, _)| o == s)
            {
                out.push((s.to_string(), None));
            }
        }
        out
    }

    /// Issues from a list: JSON objects, or the markdown list Sentry's
    /// server prints (`## 1. [SHORT-ID](url)` then the title and fields).
    fn issues(v: &Value, org: &str) -> Vec<Found> {
        let json_issues = list(v, &["issues", "results", "data"]);
        if !json_issues.is_empty() {
            return json_issues
                .into_iter()
                .filter_map(|i| Sentry::from_json(i, org))
                .collect();
        }
        v.as_str()
            .map(|t| Sentry::from_markdown(t, org))
            .unwrap_or_default()
    }

    fn from_json(i: &Value, org: &str) -> Option<Found> {
        let short = i["shortId"].as_str()?;
        let title = i["title"].as_str().unwrap_or(short);
        let status = i["status"].as_str().unwrap_or("unresolved");
        let project = i["projectSlug"]
            .as_str()
            .map(str::to_string)
            .or_else(|| i["project"]["slug"].as_str().map(str::to_string))
            .or_else(|| name_of(&i["project"]));
        let mut fields = vec![];
        if let Some(c) = i["culprit"].as_str().filter(|c| !c.is_empty()) {
            fields.push(Field::new("Where", c));
        }
        let number = |keys: &[&str]| {
            keys.iter().find_map(|k| match &i[*k] {
                Value::Number(n) => Some(n.to_string()),
                Value::String(s) if !s.is_empty() => Some(s.clone()),
                _ => None,
            })
        };
        if let Some(n) = number(&["occurrences", "count"]) {
            fields.push(Field::new("Events", n));
        }
        if let Some(n) = number(&["usersImpacted", "userCount"]) {
            fields.push(Field::new("Users", n));
        }
        Some(Sentry::event(
            org,
            short,
            title,
            i["url"]
                .as_str()
                .or_else(|| i["permalink"].as_str())
                .map(str::to_string),
            status,
            project,
            time_of(&i["lastSeen"]),
            fields,
        ))
    }

    fn from_markdown(text: &str, org: &str) -> Vec<Found> {
        let mut out = Vec::new();
        // (short id, url, the lines after it)
        let mut cur: Option<(String, Option<String>, Vec<String>)> = None;
        let flush = |cur: &mut Option<(String, Option<String>, Vec<String>)>,
                     out: &mut Vec<Found>| {
            let Some((short, url, lines)) = cur.take() else {
                return;
            };
            let mut title = None;
            let mut fields = Vec::new();
            let (mut status, mut project, mut last_seen) = ("unresolved".to_string(), None, None);
            for l in &lines {
                let l = l.trim();
                if l.is_empty() {
                    continue;
                }
                let kv = l
                    .trim_start_matches("- ")
                    .strip_prefix("**")
                    .and_then(|r| r.split_once("**"))
                    .map(|(k, v)| {
                        (
                            k.trim_end_matches(':').trim(),
                            v.trim_start_matches(':').trim(),
                        )
                    });
                match kv {
                    Some((k, v)) if !v.is_empty() => match k.to_ascii_lowercase().as_str() {
                        "status" => status = v.to_ascii_lowercase(),
                        "project" => project = Some(v.trim_matches('`').to_string()),
                        "last seen" => {
                            last_seen = DateTime::parse_from_rfc3339(
                                v.split_whitespace().next().unwrap_or(v),
                            )
                            .ok()
                            .map(|t| t.with_timezone(&Utc));
                            fields.push(Field::new("Last seen", v));
                        }
                        "culprit" => fields.push(Field::new("Where", v.trim_matches('`'))),
                        "events" | "occurrences" => fields.push(Field::new("Events", v)),
                        "users" | "users impacted" => fields.push(Field::new("Users", v)),
                        _ => {}
                    },
                    // The first plain line under the heading is the title.
                    Some((k, _)) if title.is_none() => title = Some(k.to_string()),
                    None if title.is_none() && !l.starts_with('-') && !l.starts_with('#') => {
                        title = Some(l.trim_matches('*').to_string())
                    }
                    _ => {}
                }
            }
            out.push(Sentry::event(
                org,
                &short,
                title.as_deref().unwrap_or(&short),
                url,
                &status,
                project,
                last_seen,
                fields,
            ));
        };
        for line in text.lines() {
            if let Some(rest) = line.trim().strip_prefix("## ") {
                flush(&mut cur, &mut out);
                // "1. [PROJ-12](https://…)" or "PROJ-12"
                let rest = rest.split_once(". ").map_or(rest, |(n, r)| {
                    if n.chars().all(|c| c.is_ascii_digit()) {
                        r
                    } else {
                        rest
                    }
                });
                let (short, url) = match rest.strip_prefix('[').and_then(|r| r.split_once("](")) {
                    Some((s, u)) => (s.to_string(), u.split(')').next().map(str::to_string)),
                    None => (rest.trim().trim_matches('*').to_string(), None),
                };
                if looks_like_short_id(&short) {
                    cur = Some((short, url, Vec::new()));
                }
            } else if let Some((_, _, lines)) = cur.as_mut() {
                lines.push(line.to_string());
            }
        }
        flush(&mut cur, &mut out);
        out
    }

    #[allow(clippy::too_many_arguments)]
    fn event(
        org: &str,
        short: &str,
        title: &str,
        url: Option<String>,
        status: &str,
        project: Option<String>,
        last_seen: Option<DateTime<Utc>>,
        mut fields: Vec<Field>,
    ) -> Found {
        let resolved = resolved_status(status);
        fields.insert(0, Field::new("Status", status).tone(Tone::Danger));
        let url = url.or_else(|| Some(format!("https://{org}.sentry.io/issues/{short}/")));
        let mut blocks = vec![Block::Fields { items: fields }];
        if let Some(u) = &url {
            blocks.push(Block::Links {
                items: vec![Link {
                    label: "Open in Sentry".into(),
                    url: u.clone(),
                }],
            });
        }
        let entities = project
            .iter()
            .map(|p| EntityRef {
                kind: "project".into(),
                key: format!("sentry:{org}/{p}"),
                label: p.clone(),
                rel: "in".into(),
            })
            .collect();
        Found {
            resolved,
            event: IncomingEvent {
                source: String::new(),
                // Short ids are unique within an organization.
                external_id: format!("{org}/{short}"),
                kind: "sentry.issue".into(),
                title: format!("{short} {title}"),
                body: None,
                url,
                actor: None,
                context: project.clone().or_else(|| Some(org.to_string())),
                // Last seen changes on every event; the first look sets the
                // time, and only status or title changes reopen it.
                occurred_at: last_seen.unwrap_or_else(Utc::now),
                blocks,
                entities,
                metadata: json!({ "organization": org, "short_id": short }),
            },
        }
    }
}

fn resolved_status(s: &str) -> bool {
    matches!(s, "resolved" | "ignored" | "archived" | "muted")
}

/// `PROJ-12` or `PROJ-1A2`: letters, a dash, then letters and digits.
fn looks_like_short_id(s: &str) -> bool {
    let Some((a, b)) = s.rsplit_once('-') else {
        return false;
    };
    !a.is_empty()
        && !b.is_empty()
        && a.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        && b.chars().all(|c| c.is_ascii_alphanumeric())
}

#[async_trait]
impl Recipe for Sentry {
    fn kinds(&self) -> Vec<KindInfo> {
        vec![KindInfo::new(
            "sentry.issue",
            "Sentry issue",
            "alert",
            Tone::Danger,
        )]
    }

    async fn poll(
        &self,
        s: &Session<'_>,
        _since: Option<DateTime<Utc>>,
    ) -> anyhow::Result<Vec<Found>> {
        let list_tool = s
            .pick(&["search_issues", "find_issues", "list_issues"])
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Sentry didn't offer a tool to search issues; reconnect and pick the Triage or Inspect skill"
                )
            })?;
        let orgs = match s.pick(&["find_organizations", "list_organizations"]) {
            Some(t) => Sentry::orgs(&s.call(t, json!({})).await?),
            None => Vec::new(),
        };
        anyhow::ensure!(
            !orgs.is_empty(),
            "Sentry didn't say which organizations you're in"
        );
        let mut out = Vec::new();
        for (org, region) in orgs.iter().take(5) {
            // Sentry search syntax runs as written, with no AI in between.
            let mut args = json!({
                "organizationSlug": org, "query": SENTRY_QUERY, "sort": "date", "limit": 100,
            });
            if let Some(r) = region {
                args["regionUrl"] = json!(r);
            }
            let v = s.call(list_tool, args).await?;
            out.extend(
                Sentry::issues(&v, org)
                    .into_iter()
                    .filter(|f| !f.resolved)
                    .map(|mut f| {
                        if let Some(r) = region {
                            f.event.metadata["region_url"] = json!(r);
                        }
                        f
                    }),
            );
        }
        Ok(out)
    }

    async fn check(&self, s: &Session<'_>, item: &IncomingEvent) -> anyhow::Result<Option<bool>> {
        let (Some(org), Some(short)) = (
            item.metadata["organization"].as_str(),
            item.metadata["short_id"].as_str(),
        ) else {
            return Ok(None);
        };
        let mut args = json!({ "organizationSlug": org, "issueId": short });
        if let Some(r) = item.metadata["region_url"].as_str() {
            args["regionUrl"] = json!(r);
        }
        let v = match s.pick(&["get_issue_details", "get_sentry_resource"]) {
            Some("get_sentry_resource") => {
                args["resourceType"] = json!("issue");
                args["resourceId"] = json!(short);
                s.call("get_sentry_resource", args).await?
            }
            Some(t) => s.call(t, args).await?,
            None => return Ok(None),
        };
        let v = if v["issue"].is_object() {
            v["issue"].clone()
        } else {
            v
        };
        if let Some(st) = v["status"].as_str() {
            return Ok(Some(resolved_status(st)));
        }
        let text = v.as_str().unwrap_or_default().to_ascii_lowercase();
        Ok(text.lines().find(|l| l.contains("**status**")).map(|l| {
            ["resolved", "ignored", "archived", "muted"]
                .iter()
                .any(|w| l.contains(w))
        }))
    }
}

// ---------------------------------------------------------------------------
// Notion
// ---------------------------------------------------------------------------

/// Pages that match a search the person picks. Notion's server has no
/// "assigned to me", so the default search asks for it in words.
struct Notion;

const NOTION_QUERY: &str = "tasks assigned to me";

impl Notion {
    fn found(r: &Value) -> Option<Found> {
        let id = r["id"].as_str()?;
        let title = r["title"]
            .as_str()
            .filter(|t| !t.is_empty())
            .unwrap_or("Untitled");
        let url = r["url"]
            .as_str()
            .map(str::to_string)
            .or_else(|| Some(format!("https://www.notion.so/{}", id.replace('-', ""))));
        let highlight = r["highlight"].as_str().filter(|h| !h.trim().is_empty());
        let mut blocks = Vec::new();
        if let Some(h) = highlight {
            blocks.push(Block::Text {
                text: clip(h, 600),
                markdown: false,
            });
        }
        if let Some(u) = &url {
            blocks.push(Block::Links {
                items: vec![Link {
                    label: "Open in Notion".into(),
                    url: u.clone(),
                }],
            });
        }
        let kind = match r["type"].as_str() {
            Some("database") | Some("data_source") => "notion.database",
            _ => "notion.page",
        };
        Some(Found {
            resolved: false,
            event: IncomingEvent {
                source: String::new(),
                external_id: id.to_string(),
                kind: kind.into(),
                title: title.to_string(),
                body: highlight.map(|h| clip(h, 300)),
                url,
                actor: None,
                context: None,
                occurred_at: time_of(&r["page_last_edited_at"])
                    .or_else(|| time_of(&r["last_edited_time"]))
                    .or_else(|| time_of(&r["timestamp"]))
                    .unwrap_or_else(Utc::now),
                blocks,
                entities: Vec::new(),
                metadata: json!({}),
            },
        })
    }
}

#[async_trait]
impl Recipe for Notion {
    fn kinds(&self) -> Vec<KindInfo> {
        vec![
            KindInfo::new("notion.page", "Notion page", "notion", Tone::Neutral),
            KindInfo::new(
                "notion.database",
                "Notion database",
                "notion",
                Tone::Neutral,
            ),
        ]
    }

    fn interval(&self) -> Duration {
        Duration::from_secs(15 * 60)
    }

    fn query_hint(&self) -> Option<&'static str> {
        Some(NOTION_QUERY)
    }

    async fn poll(
        &self,
        s: &Session<'_>,
        _since: Option<DateTime<Utc>>,
    ) -> anyhow::Result<Vec<Found>> {
        let tool = s
            .pick(&["notion-search", "search"])
            .ok_or_else(|| anyhow::anyhow!("Notion has no search tool"))?;
        let query = s.query.unwrap_or(NOTION_QUERY);
        // Newest first where the plan allows it; Notion drops options a
        // plan doesn't have rather than failing.
        let v = s
            .call(tool, json!({ "query": query, "sort": "last_edited" }))
            .await?;
        Ok(list(&v, &["results", "pages", "data"])
            .into_iter()
            .filter_map(Notion::found)
            .take(50)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_prefers_structured_then_json_then_text() {
        let structured = json!({ "structuredContent": { "a": 1 }, "content": [{ "type": "text", "text": "x" }] });
        assert_eq!(payload(&structured), json!({ "a": 1 }));
        let text_json = json!({ "content": [{ "type": "text", "text": " [1, 2] " }] });
        assert_eq!(payload(&text_json), json!([1, 2]));
        let text = json!({ "content": [{ "type": "text", "text": "# Hi" }, { "type": "text", "text": "there" }] });
        assert_eq!(payload(&text), json!("# Hi\nthere"));
    }

    #[test]
    fn linear_maps_issues_and_spots_finished_ones() {
        let v = json!({ "issues": [
            { "id": "abc", "identifier": "ENG-12", "title": "Fix login", "url": "https://linear.app/x/issue/ENG-12",
              "status": "In Progress", "priority": { "value": 2, "name": "High" }, "team": "Engineering",
              "updatedAt": "2026-11-08T10:00:00.000Z", "description": "Steps…" },
            { "id": "def", "identifier": "ENG-13", "title": "Old", "status": "Done",
              "completedAt": "2026-11-07T10:00:00.000Z" },
            { "id": "ghi", "identifier": "ENG-14", "title": "Other", "status": { "name": "Won't do", "type": "canceled" } },
            { "title": "no id" },
        ] });
        let found: Vec<Found> = list(&v, &["issues"])
            .into_iter()
            .filter_map(Linear::found)
            .collect();
        assert_eq!(found.len(), 3);
        let a = &found[0];
        assert!(!a.resolved);
        assert_eq!(a.event.external_id, "abc");
        assert_eq!(a.event.kind, "linear.issue");
        assert_eq!(a.event.title, "ENG-12 Fix login");
        assert_eq!(a.event.context.as_deref(), Some("Engineering"));
        assert_eq!(
            a.event.occurred_at.to_rfc3339(),
            "2026-11-08T10:00:00+00:00"
        );
        assert!(found[1].resolved);
        assert!(found[2].resolved);
        // Newer servers: the key in `id`, the UUID in `uuid`, `statusType`.
        let n = Linear::found(
            &json!({ "id": "ENG-1", "uuid": "u-1", "title": "New", "status": "Shipped",
            "statusType": "completed", "archivedAt": null }),
        )
        .unwrap();
        assert_eq!(n.event.external_id, "u-1");
        assert_eq!(n.event.title, "ENG-1 New");
        assert_eq!(n.event.metadata["identifier"], "ENG-1");
        assert!(n.resolved);
        let open = Linear::found(&json!({ "id": "ENG-2", "title": "Open", "status": "Todo",
            "statusType": "unstarted", "completedAt": null }))
        .unwrap();
        assert!(!open.resolved);
        // A bare array works too.
        assert_eq!(list(&json!([{ "id": "x" }]), &["issues"]).len(), 1);
    }

    #[test]
    fn sentry_reads_organizations_and_markdown_issues() {
        let orgs = json!(
            "# Organizations\n\n## **acme**\n\n**Web URL:** https://acme.sentry.io\n\n## **side-project**\n"
        );
        let slugs = |v: &Value| {
            Sentry::orgs(v)
                .into_iter()
                .map(|(s, _)| s)
                .collect::<Vec<_>>()
        };
        assert_eq!(slugs(&orgs), vec!["acme", "side-project"]);
        assert_eq!(
            Sentry::orgs(
                &json!({ "organizations": [{ "slug": "acme", "regionUrl": "https://us.sentry.io" }], "hasMore": false })
            ),
            vec![("acme".to_string(), Some("https://us.sentry.io".to_string()))]
        );

        let md = "# Issues in **acme**\n\n## 1. [WEB-3F](https://acme.sentry.io/issues/WEB-3F)\n\n**TypeError: x is undefined**\n\n- **Status**: unresolved\n- **Project**: web\n- **Culprit**: `app/main.ts`\n- **Last Seen**: 2026-11-08T09:00:00.000Z\n\n## 2. [API-9](https://acme.sentry.io/issues/API-9)\n\nTimeout talking to db\n\n- **Status**: resolved\n";
        let found = Sentry::issues(&json!(md), "acme");
        assert_eq!(found.len(), 2);
        let a = &found[0].event;
        assert_eq!(a.external_id, "acme/WEB-3F");
        assert_eq!(a.title, "WEB-3F TypeError: x is undefined");
        assert_eq!(
            a.url.as_deref(),
            Some("https://acme.sentry.io/issues/WEB-3F")
        );
        assert_eq!(a.context.as_deref(), Some("web"));
        assert!(!found[0].resolved);
        assert_eq!(found[1].event.title, "API-9 Timeout talking to db");
        assert!(found[1].resolved);

        // search_issues' structured result.
        let j = json!({ "query": "is:unresolved assigned:me", "issues": [{ "shortId": "WEB-1", "title": "Boom",
            "url": "https://acme.sentry.io/issues/WEB-1", "status": "unresolved", "occurrences": 42, "usersImpacted": 3,
            "culprit": "app/foo.js in bar", "projectSlug": "web", "lastSeen": "2026-11-08T09:00:00Z" }] });
        let found = Sentry::issues(&j, "acme");
        let e = &found[0].event;
        assert_eq!(e.title, "WEB-1 Boom");
        assert_eq!(
            e.url.as_deref(),
            Some("https://acme.sentry.io/issues/WEB-1")
        );
        assert_eq!(e.context.as_deref(), Some("web"));
        let Block::Fields { items } = &e.blocks[0] else {
            panic!()
        };
        assert!(items.iter().any(|f| f.label == "Events" && f.value == "42"));
    }

    #[test]
    fn notion_maps_search_results() {
        let v = json!({ "results": [
            { "id": "1f2e", "title": "Launch plan", "url": "https://www.notion.so/Launch-1f2e", "type": "page",
              "highlight": "Owner: you", "timestamp": "2026-11-01T00:00:00.000Z" },
            { "id": "db1", "title": "", "type": "database" },
        ], "type": "workspace_search" });
        let found: Vec<Found> = list(&v, &["results"])
            .into_iter()
            .filter_map(Notion::found)
            .collect();
        assert_eq!(found[0].event.title, "Launch plan");
        assert_eq!(found[0].event.kind, "notion.page");
        assert_eq!(found[1].event.title, "Untitled");
        assert_eq!(found[1].event.kind, "notion.database");
        assert_eq!(
            found[1].event.url.as_deref(),
            Some("https://www.notion.so/db1")
        );
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
                        "search_issues" => {
                            assert_eq!(args["query"], SENTRY_QUERY);
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
        while lines.len() < 3 {
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
        assert!(lines[2].starts_with("status sentry: Running"), "{lines:?}");
        let q = remote.infos()[0].queue_sync.clone().unwrap();
        assert!(q.on);
        assert_eq!(q.found, Some(2));
        assert!(q.last_sync.is_some() && q.error.is_none());

        // The second look: WEB-2 dropped out of the list, so tab asks
        // about it, and it resolved.
        let (ctx, mut rx, _c) = crate::connector::test_context("sentry");
        assert_eq!(m.look(&ctx).await.unwrap(), 1);
        drop(ctx);
        let mut rest = Vec::new();
        while let Some(l) = rx.recv().await {
            rest.push(l);
        }
        assert_eq!(rest.len(), 2, "{rest:?}");
        assert!(rest[0].starts_with("event sentry.issue | acme/WEB-1"));
        assert_eq!(rest[1], "resolved acme/WEB-2");

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
