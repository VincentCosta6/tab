//! The engine wires connectors, the store, the ranker and subscribers together.
//!
//! Every state change is published as an [`EngineEvent`] with a [`Change`]
//! describing what happened. The desktop UI subscribes to these today; the
//! automations runtime and the MCP server subscribe to the same stream later,
//! so "when an item is snoozed, do X" needs no new plumbing.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Serialize;
use tokio::sync::{Semaphore, broadcast, mpsc};
use tokio_util::sync::CancellationToken;

use crate::TabConfig;
use crate::connector::{Connector, ConnectorContext, EngineMsg};
use crate::connectors;
use crate::gateway::Gateway;
use crate::gateway::GatewayError;
use crate::model::{
    Approval, ConnectorInfo, ConnectorStatus, ContextStatus, DoneGuard, GuardLevel, IncomingEvent,
    ItemState, NewTask, Preview, QueueItem, Ranking, Session, SessionMessage, SessionStatus,
};
use crate::ranker::{Pipeline, Ranker};
use crate::ratelimit;
use crate::store::{Store, Upsert};
use crate::workflow::runtime::{Host, Runtime};
use crate::workflow::{Run, Workflow};

pub use tab_domain::model::Change;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)] // items dominate the stream; boxing buys nothing
pub enum EngineEvent {
    ItemChanged {
        change: Change,
        item: QueueItem,
    },
    ConnectorChanged {
        connector: ConnectorInfo,
    },
    /// A cached preview was filled or refreshed.
    PreviewChanged {
        preview: Preview,
    },
    /// Context entries on an item were added, promoted, pinned or removed.
    ContextChanged {
        item_id: String,
    },
    SessionChanged {
        session: Session,
    },
    SessionMessage {
        message: SessionMessage,
    },
    /// Streaming text from the agent, before the full message is stored.
    SessionDelta {
        session_id: String,
        text: String,
    },
    ApprovalRequested {
        approval: Approval,
    },
    ApprovalResolved {
        session_id: String,
        id: String,
    },
    /// Settings were saved, by the person or by an agent setting up a connection.
    ConfigChanged,
    /// A project or its notes changed.
    ProjectChanged {
        project_id: String,
    },
    /// A workflow run started or finished.
    WorkflowRun {
        run: Run,
    },
    /// A workflow was saved, turned on or off, or deleted.
    WorkflowsChanged {
        id: String,
        /// `user` or `agent`.
        by: String,
    },
    /// An agent changed something through a connection's CLI (`gh pr
    /// merge`, `git push`). The app refreshes what shows it.
    CliChanged {
        session_id: String,
        change: tab_domain::CliChange,
    },
    /// The things behind these pages changed (an agent's `gh pr merge`, a
    /// connector noticing upstream). The app refreshes link previews of
    /// them, and whatever else the connection says shows them.
    PagesChanged {
        connector: String,
        pages: Vec<String>,
    },
}

const MAX_CONCURRENT_RANKS: usize = 4;
/// How often the background preview refresher looks for work.
const REFRESH_EVERY: Duration = Duration::from_secs(45);
/// Previews refreshed per pass at most, so one pass never bursts.
const REFRESH_BATCH: usize = 12;

pub struct Engine {
    pub store: Arc<Store>,
    pub gateway: Arc<Gateway>,
    pub ranker: Arc<dyn Ranker>,
    pub connectors: Vec<Arc<dyn Connector>>,
    /// Every known connector type with its initial status, for the Connections tab.
    pub infos: Vec<ConnectorInfo>,
    pub events: broadcast::Sender<EngineEvent>,
    /// The hub's leases. Without a hub, leases kept in memory.
    pub leases: Arc<dyn crate::leases::Leases>,
}

impl Engine {
    /// Uses the hub's leases, so runners sharing the hub split the work.
    pub fn with_leases(mut self, leases: Arc<dyn crate::leases::Leases>) -> Self {
        self.leases = leases;
        self
    }

    /// Adds a queue sync for every MCP connection tab has a recipe for
    /// (see [`crate::remote_mcp::sync`]).
    pub fn with_mcp(mut self, cfg: &TabConfig, remote: &Arc<crate::remote_mcp::RemoteMcp>) -> Self {
        self.connectors
            .extend(crate::remote_mcp::sync::build(cfg, remote, &self.gateway));
        self
    }

    /// Builds an engine from config: enabled connectors, ranking pipeline, connector infos.
    pub fn from_config(
        cfg: &TabConfig,
        store: Arc<Store>,
        gateway: Arc<Gateway>,
        events: broadcast::Sender<EngineEvent>,
    ) -> Self {
        let (built, failed) = connectors::build_enabled(cfg, gateway.clone());
        let infos = connectors::registry()
            .into_iter()
            .map(|f| {
                let descriptor = f.descriptor();
                let enabled = cfg.connector(&descriptor.id).is_some();
                let status = match failed.iter().find(|(id, _)| *id == descriptor.id) {
                    Some((_, message)) => ConnectorStatus::Error {
                        message: message.clone(),
                    },
                    None if built.iter().any(|c| c.id() == descriptor.id) => {
                        ConnectorStatus::Starting
                    }
                    None if cfg
                        .connector(&descriptor.id)
                        .is_some_and(|c| !c.runs_here()) =>
                    {
                        ConnectorStatus::InCloud
                    }
                    None => ConnectorStatus::Disabled,
                };
                ConnectorInfo {
                    descriptor,
                    enabled,
                    status,
                }
            })
            .collect();
        // Every connector's declared limits feed the gateway's budgets.
        for f in connectors::registry() {
            gateway.declare_limits(&f.descriptor().rate_limits);
        }
        let ranker = Arc::new(Pipeline::from_config(cfg, gateway.clone(), store.clone()));
        let me = store.device_id().unwrap_or_else(|_| "this-computer".into());
        Self {
            leases: crate::leases::InMemory::new(&me),
            store,
            gateway,
            ranker,
            connectors: built,
            infos,
            events,
        }
    }
}

/// A running engine. Dropping it does not stop it; call [`EngineHandle::shutdown`].
pub struct EngineHandle {
    core: Arc<Core>,
    workflows: Arc<Runtime>,
    cancel: CancellationToken,
}

struct Core {
    store: Arc<Store>,
    gateway: Arc<Gateway>,
    ranker: Arc<dyn Ranker>,
    events: broadcast::Sender<EngineEvent>,
    rank_slots: Semaphore,
    connectors: HashMap<String, Arc<dyn Connector>>,
    infos: Arc<Mutex<Vec<ConnectorInfo>>>,
    leases: Arc<dyn crate::leases::Leases>,
}

fn changed(core: &Core, change: Change, item: Option<QueueItem>) -> Option<QueueItem> {
    if let Some(i) = &item {
        core.publish(change, i.clone());
    }
    item
}

impl Core {
    /// Adds a line to an item's timeline. Never fails the caller.
    fn note(
        &self,
        item_id: &str,
        kind: &str,
        title: &str,
        by: Option<&str>,
        detail: serde_json::Value,
    ) {
        if let Err(e) = self
            .store
            .activity_add(item_id, Utc::now(), kind, title, by, detail)
        {
            tracing::warn!(error = %e, "recording activity failed");
        }
    }

    /// Why an item shouldn't be marked done yet: its agent still busy, and
    /// whatever each connector raises about it or its Context. A connector
    /// that errors or takes over 3s is skipped, so Done never hangs on it.
    async fn done_guards(&self, id: &str) -> anyhow::Result<Vec<DoneGuard>> {
        let Some(ctx) = self.store.item_context(id)? else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        if let Some(s) = self.store.session_for_item(id)? {
            let why = match s.status {
                SessionStatus::Working => Some("The agent is still working"),
                SessionStatus::NeedsInput => Some("The agent is waiting on you"),
                SessionStatus::Monitoring => Some("The agent is still monitoring background work"),
                SessionStatus::Idle | SessionStatus::Failed => None,
            };
            if let Some(why) = why {
                out.push(DoneGuard {
                    source: "agent".into(),
                    level: GuardLevel::Confirm,
                    title: why.into(),
                    detail: None,
                    url: None,
                });
            }
        }
        let asks = self.connectors.values().map(|c| {
            let ctx = &ctx;
            async move {
                match tokio::time::timeout(Duration::from_secs(3), c.done_guards(ctx)).await {
                    Ok(Ok(g)) => g,
                    Ok(Err(e)) => {
                        tracing::warn!(connector = c.id(), error = %e, "done_guards failed");
                        Vec::new()
                    }
                    Err(_) => {
                        tracing::warn!(connector = c.id(), "done_guards timed out");
                        Vec::new()
                    }
                }
            }
        });
        out.extend(
            futures_util::future::join_all(asks)
                .await
                .into_iter()
                .flatten(),
        );
        Ok(out)
    }

    /// Where the things in an item's Context stand, from every connector.
    /// Like [`Self::done_guards`], a connector that errors or takes over 3s
    /// is left out.
    async fn context_status(&self, id: &str) -> anyhow::Result<Vec<ContextStatus>> {
        let Some(ctx) = self.store.item_context(id)? else {
            return Ok(Vec::new());
        };
        let asks = self.connectors.values().map(|c| {
            let ctx = &ctx;
            async move {
                match tokio::time::timeout(Duration::from_secs(3), c.context_status(ctx)).await {
                    Ok(Ok(s)) => s,
                    Ok(Err(e)) => {
                        tracing::warn!(connector = c.id(), error = %e, "context_status failed");
                        Vec::new()
                    }
                    Err(_) => {
                        tracing::warn!(connector = c.id(), "context_status timed out");
                        Vec::new()
                    }
                }
            }
        });
        Ok(futures_util::future::join_all(asks)
            .await
            .into_iter()
            .flatten()
            .collect())
    }

    async fn mark_done(&self, id: &str, by: &str) -> anyhow::Result<Option<QueueItem>> {
        if let Some(g) = self
            .done_guards(id)
            .await?
            .into_iter()
            .find(|g| g.level == GuardLevel::Block)
        {
            anyhow::bail!("can't mark done yet: {}", g.title);
        }
        let item = changed(
            self,
            Change::Done,
            self.store
                .set_state(id, ItemState::Done, None, Utc::now())?,
        );
        if let Some(i) = &item
            && let Some(c) = self.connectors.get(&i.event.source)
            && let Err(e) = c.on_done(i).await
        {
            tracing::warn!(connector = c.id(), error = %e, "on_done failed");
        }
        if item.is_some() {
            self.note(id, "done", "Marked done", Some(by), serde_json::json!({}));
        }
        Ok(item)
    }

    fn snooze(
        &self,
        id: &str,
        until: DateTime<Utc>,
        by: &str,
    ) -> anyhow::Result<Option<QueueItem>> {
        let item = changed(
            self,
            Change::Snoozed,
            self.store
                .set_state(id, ItemState::Snoozed, Some(until), Utc::now())?,
        );
        if item.is_some() {
            self.note(
                id,
                "snoozed",
                "Snoozed",
                Some(by),
                serde_json::json!({ "until": until }),
            );
        }
        Ok(item)
    }

    fn reopen(&self, id: &str, by: &str) -> anyhow::Result<Option<QueueItem>> {
        let item = changed(
            self,
            Change::Reopened,
            self.store
                .set_state(id, ItemState::Open, None, Utc::now())?,
        );
        if item.is_some() {
            self.note(id, "reopened", "Reopened", Some(by), serde_json::json!({}));
        }
        Ok(item)
    }

    /// `chain` is set when a workflow creates the task, so workflows it
    /// starts know where it came from.
    fn create_task(
        self: &Arc<Self>,
        task: NewTask,
        chain: &[String],
        workflow: Option<&str>,
    ) -> anyhow::Result<QueueItem> {
        let now = Utc::now();
        let title = task.title.trim();
        anyhow::ensure!(!title.is_empty(), "a task needs a title");
        let scheduled = task.due_at.is_some_and(|d| d > now);
        let mut metadata = serde_json::json!({ "created_by": task.created_by });
        if let Some(p) = task.priority.as_deref().filter(|p| *p != "normal") {
            metadata["priority"] = p.into();
        }
        if let Some(d) = task.due_at {
            metadata["due_at"] = d.to_rfc3339().into();
        }
        if !chain.is_empty() {
            metadata["workflow_chain"] = serde_json::json!(chain);
        }
        if let Some(w) = workflow {
            metadata["workflow"] = w.into();
        }
        if task.auto_title {
            metadata["title_source"] = "auto".into();
        }
        let event = IncomingEvent {
            source: crate::connectors::tab::SOURCE.into(),
            external_id: uuid::Uuid::new_v4().to_string(),
            kind: if scheduled {
                "tab.scheduled"
            } else {
                "tab.task"
            }
            .into(),
            title: title.chars().take(200).collect(),
            body: task.body.filter(|b| !b.trim().is_empty()),
            url: task.url.filter(|u| !u.trim().is_empty()),
            actor: Some(
                match task.created_by.as_str() {
                    "agent" => "agent",
                    "workflow" => "workflow",
                    _ => "you",
                }
                .into(),
            ),
            context: Some("tab".into()),
            occurred_at: now,
            metadata,
            ..Default::default()
        };
        let item = self
            .ingest(&event)?
            .ok_or_else(|| anyhow::anyhow!("task was not stored"))?;
        let item = match task.project_id.as_deref().filter(|p| !p.is_empty()) {
            Some(p) => self
                .store
                .item_set_project(&item.id, Some(p), now)?
                .unwrap_or(item),
            None => item,
        };
        if scheduled && let Some(due) = task.due_at {
            return Ok(self.snooze(&item.id, due, "schedule")?.unwrap_or(item));
        }
        Ok(item)
    }

    /// Marks an item done because it finished at the source.
    fn finish_upstream(&self, item: &QueueItem) {
        match self
            .store
            .set_state(&item.id, ItemState::Done, None, Utc::now())
        {
            Ok(Some(i)) => {
                tracing::info!(id = %i.id, title = %i.event.title, "finished upstream");
                let title = match i.event.metadata["subject_type"].as_str() {
                    Some("PullRequest") => "Pull request merged or closed",
                    Some("Issue") => "Issue closed",
                    _ => "Finished upstream",
                };
                self.note(
                    &i.id,
                    "finished_upstream",
                    title,
                    Some("upstream"),
                    serde_json::json!({}),
                );
                self.publish(Change::Done, i);
            }
            Ok(None) => {}
            Err(e) => tracing::error!(error = %e, "marking a finished item done failed"),
        }
    }

    fn publish(&self, change: Change, item: QueueItem) {
        let _ = self.events.send(EngineEvent::ItemChanged { change, item });
    }

    async fn rank(self: Arc<Self>, item: QueueItem) {
        let Ok(_slot) = self.rank_slots.acquire().await else {
            return;
        };
        match self.ranker.rank(&item).await {
            Ok(mut ranking) => match self
                .under_parent(&item.id, &mut ranking)
                .and_then(|()| self.store.set_ranking(&item.id, &ranking))
            {
                Ok(Some(item)) => self.publish(Change::Ranked, item),
                Ok(None) => {}
                Err(e) => tracing::error!(error = %e, "saving ranking failed"),
            },
            Err(e) => {
                tracing::warn!(item = %item.id, error = %e, "ranking failed; item stays unranked")
            }
        }
    }

    /// A follow-up never ranks above the open task it came from: ranked on
    /// its own, a later phase can look more urgent than the work it builds on.
    fn under_parent(&self, id: &str, ranking: &mut Ranking) -> anyhow::Result<()> {
        // Read again: links are added just after a task is created.
        let Some(parent) = self.store.get(id)?.and_then(|i| i.parent_id) else {
            return Ok(());
        };
        let Some(parent) = self
            .store
            .get(&parent)?
            .filter(|p| p.state != ItemState::Done)
        else {
            return Ok(());
        };
        if let Some(cap) = parent.ranking.filter(|p| p.score < ranking.score) {
            ranking.score = cap.score;
            ranking.urgency = cap.urgency;
            ranking.reason = format!(
                "Follow-up of \"{}\". {}",
                parent.event.title, ranking.reason
            );
        }
        Ok(())
    }

    /// Stores an event and ranks it. Shared by connectors and tab's own tasks.
    fn ingest(self: &Arc<Self>, e: &IncomingEvent) -> anyhow::Result<Option<QueueItem>> {
        Ok(match self.store.upsert(e, Utc::now())? {
            Upsert::Inserted(item) => {
                let (title, by) = match e.metadata["workflow"].as_str() {
                    Some(w) => (
                        "Created by a workflow".to_string(),
                        Some(format!("workflow:{w}")),
                    ),
                    None if e.source == crate::connectors::tab::SOURCE => {
                        ("Created".to_string(), e.actor.clone())
                    }
                    None => (format!("Arrived from {}", e.source), e.actor.clone()),
                };
                self.note(
                    &item.id,
                    "arrived",
                    &title,
                    by.as_deref(),
                    serde_json::json!({ "kind": e.kind }),
                );
                self.publish(Change::Ingested, item.clone());
                tokio::spawn(self.clone().rank(item.clone()));
                Some(item)
            }
            Upsert::Updated(item) => {
                self.note(
                    &item.id,
                    "updated",
                    &format!("Updated: {}", e.title),
                    e.actor.as_deref(),
                    serde_json::json!({ "kind": e.kind }),
                );
                // Keep showing the old preview; the refresher replaces it.
                self.store.preview_mark_dirty(&item.id)?;
                self.publish(Change::Updated, item.clone());
                tokio::spawn(self.clone().rank(item.clone()));
                Some(item)
            }
            Upsert::Unchanged => None,
        })
    }

    /// The things behind `pages` changed upstream. The connector drops
    /// what it cached about them, queue items at those pages get a fresh
    /// preview (sent as [`EngineEvent::PreviewChanged`]), and the app hears
    /// [`EngineEvent::PagesChanged`] to refresh link previews showing them.
    fn pages_changed(self: &Arc<Self>, connector: &str, pages: &[String]) {
        if pages.is_empty() {
            return;
        }
        crate::connectors::forget_pages(connector, pages);
        for page in pages {
            let Ok(Some(item)) = self.store.item_by_url(page) else {
                continue;
            };
            let Some(c) = self.connectors.get(&item.event.source).cloned() else {
                continue;
            };
            let core = self.clone();
            tokio::spawn(async move {
                core.refresh_preview(&c, &item).await;
            });
        }
        let _ = self.events.send(EngineEvent::PagesChanged {
            connector: connector.to_string(),
            pages: pages.to_vec(),
        });
    }

    /// Fetches and caches one preview. Returns `None` when the call was
    /// deferred or refused by a rate budget; the old cache stays.
    async fn refresh_preview(
        &self,
        connector: &Arc<dyn Connector>,
        item: &QueueItem,
    ) -> Option<Preview> {
        match connector.preview(item).await {
            Ok(blocks) => {
                // Millisecond precision, matching what the store keeps.
                let now = chrono::SubsecRound::trunc_subsecs(Utc::now(), 3);
                if let Err(e) = self.store.preview_put(&item.id, &blocks, now) {
                    tracing::error!(error = %e, "caching preview failed");
                }
                let preview = Preview {
                    item_id: item.id.clone(),
                    blocks,
                    fetched_at: Some(now),
                    stale: false,
                };
                let _ = self.events.send(EngineEvent::PreviewChanged {
                    preview: preview.clone(),
                });
                Some(preview)
            }
            Err(e) => {
                let budget = matches!(
                    e.downcast_ref::<GatewayError>(),
                    Some(GatewayError::Deferred { .. } | GatewayError::RateLimited { .. })
                );
                if !budget {
                    tracing::warn!(item = %item.id, error = %e, "preview failed");
                }
                None
            }
        }
    }

    fn rank_pending(self: &Arc<Self>) {
        match self.store.unranked() {
            Ok(items) => items
                .into_iter()
                .for_each(|i| drop(tokio::spawn(self.clone().rank(i)))),
            Err(e) => tracing::error!(error = %e, "loading unranked items failed"),
        }
    }
}

/// Writes agent sessions and workflow runs into item timelines.
struct Timeline {
    core: Arc<Core>,
    /// Session id → last status written, so each turn isn't a line.
    sessions: HashMap<String, crate::model::SessionStatus>,
}

impl Timeline {
    fn record(&mut self, e: &EngineEvent) {
        use crate::model::SessionStatus;
        match e {
            EngineEvent::SessionChanged { session: s } => {
                let Some(item) = &s.item_id else { return };
                let agent = crate::agents::catalog::harness(s.agent).name.as_str();
                let before = self.sessions.insert(s.id.clone(), s.status);
                let first = before.is_none()
                    && !self
                        .core
                        .store
                        .activity_for(item, 500)
                        .unwrap_or_default()
                        .iter()
                        .any(|a| {
                            a.kind == "session_started" && a.detail["session_id"] == s.id.as_str()
                        });
                let detail = serde_json::json!({ "session_id": s.id, "agent": s.agent });
                if first {
                    self.core.note(
                        item,
                        "session_started",
                        &format!("{agent} session started"),
                        Some("agent"),
                        detail.clone(),
                    );
                }
                if before != Some(s.status) {
                    match s.status {
                        SessionStatus::NeedsInput => self.core.note(
                            item,
                            "session_needs_input",
                            &format!("{agent} is waiting on you"),
                            Some("agent"),
                            detail,
                        ),
                        SessionStatus::Failed => self.core.note(
                            item,
                            "session_failed",
                            &format!("{agent} session failed"),
                            Some("agent"),
                            detail,
                        ),
                        _ => {}
                    }
                }
            }
            EngineEvent::WorkflowRun { run } if run.finished_at.is_some() && !run.dry_run => {
                use crate::workflow::RunStatus;
                let Some(item) = &run.item_id else { return };
                let declined = run.status == RunStatus::Stopped
                    && run
                        .steps
                        .last()
                        .is_some_and(|s| s.status == crate::workflow::StepStatus::Blocked);
                let outcome = match run.status {
                    _ if declined => "was declined",
                    RunStatus::Succeeded => "ran",
                    RunStatus::Stopped => "stopped at a filter",
                    RunStatus::Failed => "failed",
                    RunStatus::Blocked => "is waiting for your OK",
                    RunStatus::Throttled => "was held back by a rate limit",
                    RunStatus::LoopStopped => "was stopped to prevent a loop",
                    RunStatus::Running | RunStatus::Waiting => return,
                };
                // A filter that didn't match on the first step is noise.
                if run.status == RunStatus::Stopped && run.steps.len() <= 1 && !declined {
                    return;
                }
                self.core.note(
                    item,
                    "workflow_run",
                    &format!("Workflow \"{}\" {outcome}", run.workflow_name),
                    Some(&format!("workflow:{}", run.workflow_name)),
                    serde_json::json!({ "run_id": run.id, "workflow_id": run.workflow_id, "status": run.status, "error": run.error }),
                );
            }
            _ => {}
        }
    }
}

/// What workflows reach through the engine.
struct WorkflowHost(Arc<Core>);

fn str_arg(v: &serde_json::Value, k: &str) -> Option<String> {
    match &v[k] {
        serde_json::Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn num_arg(v: &serde_json::Value, k: &str) -> Option<i64> {
    v[k].as_i64()
        .or_else(|| v[k].as_f64().map(|f| f as i64))
        .or_else(|| v[k].as_str().and_then(|s| s.trim().parse().ok()))
}

#[async_trait]
impl Host for WorkflowHost {
    fn store(&self) -> &Store {
        &self.0.store
    }
    fn catalog(&self) -> Vec<ConnectorInfo> {
        self.0
            .infos
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    fn connector(&self, id: &str) -> Option<Arc<dyn Connector>> {
        self.0.connectors.get(id).cloned()
    }
    fn publish(&self, e: EngineEvent) {
        let _ = self.0.events.send(e);
    }
    fn leases(&self) -> Arc<dyn crate::leases::Leases> {
        self.0.leases.clone()
    }
    async fn tab_action(
        &self,
        action: &str,
        input: &serde_json::Value,
        chain: &[String],
        workflow: &str,
    ) -> anyhow::Result<(serde_json::Value, Option<String>)> {
        let core = &self.0;
        let by = format!("workflow:{workflow}");
        let item_id =
            || str_arg(input, "item_id").ok_or_else(|| anyhow::anyhow!("item_id is required"));
        let project = |name: Option<String>| -> anyhow::Result<Option<String>> {
            match name {
                None => Ok(None),
                Some(n) => Ok(Some(
                    core.store
                        .project_find(&n)?
                        .ok_or_else(|| anyhow::anyhow!("no project called {n}"))?
                        .id,
                )),
            }
        };
        let gone = |id: &str| anyhow::anyhow!("no item {id}");
        match action {
            "task.create" => {
                let due = num_arg(input, "due_in_minutes")
                    .filter(|m| *m > 0)
                    .map(|m| Utc::now() + chrono::Duration::minutes(m));
                let task = NewTask {
                    title: str_arg(input, "title").unwrap_or_default(),
                    body: str_arg(input, "body"),
                    priority: str_arg(input, "priority"),
                    due_at: due,
                    url: str_arg(input, "url"),
                    created_by: "workflow".into(),
                    project_id: project(str_arg(input, "project"))?,
                    auto_title: false,
                };
                let item = core.create_task(task, chain, Some(workflow))?;
                Ok((serde_json::json!({ "item_id": item.id }), Some(item.id)))
            }
            "item.snooze" => {
                let id = item_id()?;
                let minutes = num_arg(input, "minutes").filter(|m| *m > 0).unwrap_or(240);
                core.snooze(&id, Utc::now() + chrono::Duration::minutes(minutes), &by)?
                    .ok_or_else(|| gone(&id))?;
                Ok((serde_json::json!({}), Some(id)))
            }
            "item.done" => {
                let id = item_id()?;
                core.mark_done(&id, &by).await?.ok_or_else(|| gone(&id))?;
                Ok((serde_json::json!({}), Some(id)))
            }
            "item.reopen" => {
                let id = item_id()?;
                core.reopen(&id, &by)?.ok_or_else(|| gone(&id))?;
                Ok((serde_json::json!({}), Some(id)))
            }
            "item.set_project" => {
                let id = item_id()?;
                let p = project(str_arg(input, "project"))?;
                core.store
                    .item_set_project(&id, p.as_deref(), Utc::now())?
                    .ok_or_else(|| gone(&id))?;
                let name = str_arg(input, "project");
                core.note(
                    &id,
                    "project",
                    &match &name {
                        Some(n) => format!("Put in {n}"),
                        None => "Taken out of its project".into(),
                    },
                    Some(&by),
                    serde_json::json!({ "project_id": p }),
                );
                Ok((serde_json::json!({ "project_id": p }), Some(id)))
            }
            other => anyhow::bail!("tab has no action {other}"),
        }
    }
}

impl Engine {
    /// Spawns connectors, the ingest loop and the timers on the current Tokio runtime.
    pub fn start(self) -> EngineHandle {
        let cancel = CancellationToken::new();
        let (tx, mut rx) = mpsc::channel::<EngineMsg>(256);
        let infos = Arc::new(Mutex::new(self.infos));
        let connector_map: HashMap<String, Arc<dyn Connector>> = self
            .connectors
            .iter()
            .map(|c| (c.id().to_string(), c.clone()))
            .collect();
        let core = Arc::new(Core {
            store: self.store,
            gateway: self.gateway,
            ranker: self.ranker,
            events: self.events,
            rank_slots: Semaphore::new(MAX_CONCURRENT_RANKS),
            connectors: connector_map,
            infos: infos.clone(),
            leases: self.leases,
        });

        // Workflows listen before anything can happen.
        let workflows = Runtime::new(Arc::new(WorkflowHost(core.clone())));
        if let Err(e) = core
            .store
            .workflow_runs_abandon(&core.leases.holder(), Utc::now())
        {
            tracing::warn!(error = %e, "couldn't close unfinished workflow runs");
        }
        {
            let (wf, mut sub, wf_cancel) =
                (workflows.clone(), core.events.subscribe(), cancel.clone());
            let mut timeline = Timeline {
                core: core.clone(),
                sessions: HashMap::new(),
            };
            tokio::spawn(async move {
                loop {
                    let e = tokio::select! {
                        e = sub.recv() => e,
                        _ = wf_cancel.cancelled() => break,
                    };
                    match e {
                        Ok(e) => {
                            timeline.record(&e);
                            wf.on_event(&e)
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!(missed = n, "workflows fell behind the event stream")
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            });
        }

        // Once per start: close items whose PR (or similar) finished while tab
        // wasn't looking. Runs in the background, one item at a time.
        for c in &self.connectors {
            let (c, core, cancel) = (c.clone(), core.clone(), cancel.clone());
            tokio::spawn(async move {
                let Ok(items) = core.store.list(&[ItemState::Open, ItemState::Snoozed]) else {
                    return;
                };
                for item in items.into_iter().filter(|i| i.event.source == c.id()) {
                    if cancel.is_cancelled() {
                        return;
                    }
                    match c.is_resolved(&item).await {
                        Ok(true) => core.finish_upstream(&item),
                        Ok(false) => {}
                        Err(e) => {
                            tracing::debug!(connector = c.id(), error = %e, "resolve check failed")
                        }
                    }
                }
            });
        }

        for c in &self.connectors {
            let c = c.clone();
            let ctx = ConnectorContext::new(c.id().to_string(), tx.clone(), cancel.child_token());
            tokio::spawn(async move {
                let status = match c.run(ctx.clone()).await {
                    Ok(()) => ConnectorStatus::Stopped,
                    Err(e) => {
                        tracing::error!(connector = c.id(), error = %e, "connector failed");
                        ConnectorStatus::Error {
                            message: e.to_string(),
                        }
                    }
                };
                ctx.set_status(status).await;
            });
        }
        drop(tx);

        // Anything left unranked from a previous run (or after a rules change).
        core.rank_pending();

        let (ingest, ingest_infos, ingest_cancel) = (core.clone(), infos.clone(), cancel.clone());
        tokio::spawn(async move {
            loop {
                let msg = tokio::select! {
                    m = rx.recv() => match m { Some(m) => m, None => break },
                    _ = ingest_cancel.cancelled() => break,
                };
                match msg {
                    EngineMsg::Event(e) => {
                        if let Err(e) = ingest.ingest(&e) {
                            tracing::error!(error = %e, "storing event failed");
                        }
                    }
                    EngineMsg::Resolved(source, external_id) => {
                        match ingest.store.find_active(&source, &external_id) {
                            Ok(Some(item)) => ingest.finish_upstream(&item),
                            Ok(None) => {}
                            Err(e) => {
                                tracing::error!(error = %e, "looking up a resolved item failed")
                            }
                        }
                    }
                    EngineMsg::Changed(id, pages) => ingest.pages_changed(&id, &pages),
                    EngineMsg::Status(id, status) => {
                        let mut all = ingest_infos.lock().unwrap_or_else(|e| e.into_inner());
                        if let Some(info) = all.iter_mut().find(|c| c.descriptor.id == id)
                            && info.status != status
                        {
                            info.status = status;
                            let _ = ingest.events.send(EngineEvent::ConnectorChanged {
                                connector: info.clone(),
                            });
                        }
                    }
                }
            }
        });

        let (timer, timer_cancel, timer_wf) = (core.clone(), cancel.clone(), workflows.clone());
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(30));
            let mut n = 0u64;
            loop {
                tokio::select! {
                    _ = tick.tick() => {}
                    _ = timer_cancel.cancelled() => break,
                }
                match timer.store.wake_due(Utc::now()) {
                    Ok(items) => items.into_iter().for_each(|i| {
                        timer.note(
                            &i.id,
                            "woke",
                            "Back in the queue",
                            None,
                            serde_json::json!({}),
                        );
                        timer.publish(Change::Woke, i)
                    }),
                    Err(e) => tracing::error!(error = %e, "waking snoozed items failed"),
                }
                timer_wf.tick(Utc::now());
                if n.is_multiple_of(120) {
                    if let Err(e) = timer.store.prune(Utc::now(), chrono::Duration::days(30)) {
                        tracing::error!(error = %e, "pruning failed");
                    }
                    // Rolls up the run log's complete days before pruning it.
                    if let Err(e) = crate::workflow::rollups::maintain(&timer.store, Utc::now())
                        .and_then(|_| timer.store.activity_prune())
                    {
                        tracing::error!(error = %e, "rolling up and pruning workflow runs failed");
                    }
                }
                n += 1;
            }
        });

        // Background preview refresher. Runs every call at background
        // priority, so the gateway defers it whenever a connector's budget
        // is tight or it has been bursting; interactive opens always win.
        let (refresh, refresh_cancel) = (core.clone(), cancel.clone());
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(REFRESH_EVERY);
            loop {
                tokio::select! {
                    _ = tick.tick() => {}
                    _ = refresh_cancel.cancelled() => break,
                }
                let cutoff = Utc::now() - chrono::Duration::minutes(5);
                let due = match refresh.store.previews_due(cutoff, REFRESH_BATCH * 4) {
                    Ok(d) => d,
                    Err(e) => {
                        tracing::error!(error = %e, "listing stale previews failed");
                        continue;
                    }
                };
                let mut backed_off = std::collections::HashSet::new();
                let mut done = 0;
                for (item, fetched_at, dirty) in due {
                    if done >= REFRESH_BATCH || refresh_cancel.is_cancelled() {
                        break;
                    }
                    let Some(c) = refresh.connectors.get(&item.event.source) else {
                        continue;
                    };
                    if backed_off.contains(&item.event.source) {
                        continue;
                    }
                    let ttl = chrono::Duration::from_std(c.preview_ttl()).unwrap_or_default();
                    if !dirty && fetched_at.is_some_and(|t| Utc::now() < t + ttl) {
                        continue;
                    }
                    done += 1;
                    let ok = ratelimit::background(refresh.refresh_preview(c, &item)).await;
                    if ok.is_none() {
                        backed_off.insert(item.event.source.clone());
                    }
                }
            }
        });

        EngineHandle {
            core,
            workflows,
            cancel,
        }
    }
}

impl EngineHandle {
    pub fn store(&self) -> &Store {
        &self.core.store
    }

    pub fn gateway(&self) -> &Gateway {
        &self.core.gateway
    }

    pub fn connectors(&self) -> Vec<ConnectorInfo> {
        self.core
            .infos
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Why an item shouldn't be marked done yet. `Confirm` guards are for the
    /// UI to ask about; `Block` guards also make [`Self::mark_done`] refuse.
    pub async fn done_guards(&self, id: &str) -> anyhow::Result<Vec<DoneGuard>> {
        self.core.done_guards(id).await
    }

    /// Where the things in an item's Context stand ("In review", "Merged"),
    /// as each connector sees them.
    pub async fn context_status(&self, id: &str) -> anyhow::Result<Vec<ContextStatus>> {
        self.core.context_status(id).await
    }

    /// [`Self::context_status`] for several items at once, for the queue.
    /// Items with nothing to report, or whose lookup failed, are left out.
    pub async fn context_statuses(&self, ids: &[String]) -> HashMap<String, Vec<ContextStatus>> {
        let all =
            futures_util::future::join_all(ids.iter().map(|id| self.core.context_status(id))).await;
        ids.iter()
            .zip(all)
            .filter_map(|(id, s)| Some((id.clone(), s.ok()?)))
            .filter(|(_, s)| !s.is_empty())
            .collect()
    }

    /// Marks an item done and lets its connector react (e.g. mark the GitHub thread read).
    pub async fn mark_done(&self, id: &str) -> anyhow::Result<Option<QueueItem>> {
        self.core.mark_done(id, "you").await
    }

    pub fn snooze(&self, id: &str, until: DateTime<Utc>) -> anyhow::Result<Option<QueueItem>> {
        self.core.snooze(id, until, "you")
    }

    /// The person names an item. From then on the titler leaves it alone.
    pub fn rename(&self, id: &str, title: &str) -> anyhow::Result<Option<QueueItem>> {
        let title: String = title.split_whitespace().collect::<Vec<_>>().join(" ");
        anyhow::ensure!(!title.is_empty(), "a title can't be empty");
        let title: String = title.chars().take(200).collect();
        let Some(item) = self.core.store.get(id)? else {
            return Ok(None);
        };
        if item.event.title == title {
            return Ok(Some(item));
        }
        let mut meta = item.event.metadata.clone();
        if !meta.is_object() {
            meta = serde_json::json!({});
        }
        meta["title_source"] = "user".into();
        let now = Utc::now();
        let updated = self.core.store.item_set_title(id, &title, &meta, now)?;
        self.core.note(
            id,
            "retitled",
            &format!("Renamed “{title}”"),
            Some("you"),
            serde_json::json!({ "from": item.event.title, "to": title }),
        );
        if let Some(i) = &updated {
            self.emit(EngineEvent::ItemChanged {
                change: Change::Updated,
                item: i.clone(),
            });
        }
        Ok(updated)
    }

    pub fn reopen(&self, id: &str) -> anyhow::Result<Option<QueueItem>> {
        self.core.reopen(id, "you")
    }

    /// Runs a workflow now and waits for it: dry (nothing changes) or for
    /// real, on an item if given. A dry run doesn't need the workflow saved.
    pub async fn run_workflow(
        &self,
        w: Workflow,
        item_id: Option<&str>,
        dry_run: bool,
    ) -> anyhow::Result<Run> {
        anyhow::ensure!(
            dry_run || !w.id.is_empty(),
            "save the workflow before running it for real"
        );
        let item = match item_id.filter(|i| !i.is_empty()) {
            Some(id) => Some(
                self.core
                    .store
                    .get(id)?
                    .ok_or_else(|| anyhow::anyhow!("no item {id}"))?,
            ),
            None => None,
        };
        self.workflows.run_now(w, item, dry_run).await
    }

    /// Runs a failed run again, as the person asked; the new run notes
    /// which one it re-runs.
    pub async fn rerun_workflow(&self, run_id: &str) -> anyhow::Result<Run> {
        self.workflows.rerun(run_id).await
    }

    /// Starts an enabled workflow for an agent (see
    /// [`Runtime::run_for_agent`]), on an item if given.
    pub async fn run_workflow_for_agent(
        &self,
        workflow_id: &str,
        item_id: Option<&str>,
        session: Option<&str>,
    ) -> anyhow::Result<crate::workflow::runtime::AgentRun> {
        let item = match item_id.filter(|i| !i.is_empty()) {
            Some(id) => Some(
                self.core
                    .store
                    .get(id)?
                    .ok_or_else(|| anyhow::anyhow!("no item {id}"))?,
            ),
            None => None,
        };
        self.workflows
            .run_for_agent(workflow_id, item, session, Utc::now())
            .await
    }

    /// The person's answer to a run blocked on a step: approve that step on
    /// that run and carry on, or decline and end it. Only for a person's
    /// action in the app, never an agent's.
    pub async fn answer_workflow_run(&self, run_id: &str, approve: bool) -> anyhow::Result<Run> {
        self.workflows.answer_blocked(run_id, approve).await
    }

    /// Rich content for an item. Cached content is returned immediately,
    /// however old; if it is past the connector's TTL a background refresh
    /// is queued and arrives as [`EngineEvent::PreviewChanged`]. Only a
    /// never-seen item waits on the network, at interactive priority.
    pub async fn preview(&self, id: &str) -> anyhow::Result<Preview> {
        let empty = Preview {
            item_id: id.to_string(),
            blocks: Vec::new(),
            fetched_at: None,
            stale: false,
        };
        let Some(item) = self.core.store.get(id)? else {
            return Ok(empty);
        };
        let Some(c) = self.core.connectors.get(&item.event.source).cloned() else {
            return Ok(empty);
        };
        if let Some((blocks, fetched_at, dirty)) = self.core.store.preview_get(id)? {
            let ttl = chrono::Duration::from_std(c.preview_ttl()).unwrap_or_default();
            let stale = dirty || Utc::now() >= fetched_at + ttl;
            if stale {
                let core = self.core.clone();
                tokio::spawn(async move {
                    ratelimit::background(core.refresh_preview(&c, &item)).await;
                });
            }
            return Ok(Preview {
                item_id: id.to_string(),
                blocks,
                fetched_at: Some(fetched_at),
                stale,
            });
        }
        Ok(self.core.refresh_preview(&c, &item).await.unwrap_or(empty))
    }

    /// Something changed the things behind `pages` (an agent merged a pull
    /// request through `gh`): see [`Core::pages_changed`].
    pub fn pages_changed(&self, connector: &str, pages: &[String]) {
        self.core.pages_changed(connector, pages);
    }

    /// Creates a task in tab's own connector. A due time makes it a
    /// scheduled task that sleeps until then.
    pub fn create_task(&self, task: NewTask) -> anyhow::Result<QueueItem> {
        self.core.create_task(task, &[], None)
    }

    /// Publishes an event on the engine's stream (used by the agent runner).
    pub fn emit(&self, event: EngineEvent) {
        let _ = self.core.events.send(event);
    }

    /// Recomputes every open ranking, e.g. after the rules or instructions change.
    /// AI verdicts for unchanged inputs come from cache.
    pub fn rerank_all(&self) -> anyhow::Result<()> {
        self.core.store.clear_rankings()?;
        self.core.rank_pending();
        Ok(())
    }

    pub fn shutdown(&self) {
        self.cancel.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TabConfig;
    use crate::model::{IncomingEvent, Urgency};
    use crate::ranker::{Ranker, RankingRules};
    use async_trait::async_trait;

    struct OneShot;

    #[async_trait]
    impl Connector for OneShot {
        fn id(&self) -> &str {
            "github"
        }
        async fn run(&self, ctx: ConnectorContext) -> anyhow::Result<()> {
            ctx.emit(IncomingEvent {
                source: "github".into(),
                external_id: "42".into(),
                kind: "github.review_requested".into(),
                title: "Review: fix prod outage in billing".into(),
                actor: Some("dana".into()),
                context: Some("acme/billing".into()),
                occurred_at: Utc::now(),
                ..Default::default()
            })
            .await?;
            ctx.synced().await;
            ctx.cancelled().await;
            Ok(())
        }
    }

    #[tokio::test]
    async fn event_flows_through_to_a_ranked_item_with_lifecycle_events() {
        let (events, mut rx) = broadcast::channel(16);
        let store = Arc::new(Store::in_memory().unwrap());
        let ranker: Arc<dyn Ranker> = Arc::new(Pipeline {
            rules: RankingRules::default(),
            profile: Default::default(),
            ai: None,
        });
        let handle = Engine {
            gateway: Arc::new(Gateway::new(store.clone())),
            store,
            ranker,
            connectors: vec![Arc::new(OneShot)],
            infos: vec![],
            events,
            leases: crate::leases::InMemory::new("test"),
        }
        .start();

        let mut changes = Vec::new();
        let ranked = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(EngineEvent::ItemChanged { change, item }) = rx.recv().await {
                    changes.push(change);
                    if change == Change::Ranked {
                        return item;
                    }
                }
            }
        })
        .await
        .expect("item was ranked");
        assert_eq!(changes, [Change::Ingested, Change::Ranked]);
        let r = ranked.ranking.unwrap();
        assert_eq!(
            (r.score, r.urgency, r.ranked_by.as_str()),
            (98, Urgency::Now, "rules")
        );

        let done = handle.mark_done(&ranked.id).await.unwrap().unwrap();
        assert_eq!(done.state, ItemState::Done);
        assert!(matches!(
            rx.recv().await,
            Ok(EngineEvent::ItemChanged {
                change: Change::Done,
                ..
            })
        ));
        assert!(handle.store().list(&[ItemState::Open]).unwrap().is_empty());
        handle.shutdown();
    }

    #[tokio::test]
    async fn sample_data_runs_end_to_end() {
        let (events, _) = broadcast::channel(256);
        let store = Arc::new(Store::in_memory().unwrap());
        let gateway = Arc::new(Gateway::new(store.clone()));
        let mut cfg = TabConfig::default();
        assert!(cfg.connector("demo").is_none(), "sample data is opt-in");
        // Rules only: the AI layer would reach for a real Claude login.
        cfg.ranking.ai.enabled = false;
        cfg.connectors
            .insert("demo".into(), crate::config::ConnectorConfig::on());
        let engine = Engine::from_config(&cfg, store.clone(), gateway, events);
        assert!(
            engine
                .infos
                .iter()
                .any(|i| i.descriptor.id == "github" && !i.enabled)
        );
        let handle = engine.start();

        let ranked = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let items = handle.store().list(&[ItemState::Open]).unwrap();
                if items.len() >= 10 && items.iter().all(|i| i.ranking.is_some()) {
                    return items;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("sample items ingested and ranked");

        // Rules only (no API key): the blocked agent leads, the newsletter trails.
        assert_eq!(ranked[0].event.kind, "agent.needs_input");
        assert_eq!(ranked.last().unwrap().event.kind, "email.newsletter");
        assert!(
            ranked
                .iter()
                .all(|i| i.ranking.as_ref().unwrap().ranked_by == "rules")
        );

        let review = ranked
            .iter()
            .find(|i| i.event.kind == "github.review_requested")
            .unwrap();
        let first = handle.preview(&review.id).await.unwrap();
        assert!(first.blocks.len() >= 2 && !first.stale);
        // Second open is served from the preview cache, not the connector.
        let again = handle.preview(&review.id).await.unwrap();
        assert_eq!(again.fetched_at, first.fetched_at);

        // tab's own tasks: priority feeds ranking, a due time schedules it.
        let task = handle
            .create_task(NewTask {
                title: "Ship the onboarding flow".into(),
                priority: Some("urgent".into()),
                created_by: "user".into(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            (task.event.source.as_str(), task.event.kind.as_str()),
            ("tab", "tab.task")
        );
        let later = handle
            .create_task(NewTask {
                title: "Check the deploy".into(),
                due_at: Some(Utc::now() + chrono::Duration::hours(2)),
                created_by: "agent".into(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(later.state, ItemState::Snoozed);
        assert_eq!(later.event.kind, "tab.scheduled");
        assert!(
            !handle
                .store()
                .search("checkout", &[ItemState::Open], 5)
                .unwrap()
                .is_empty()
        );
        handle.shutdown();
    }

    /// GitHub stand-in that records the actions it's asked to run.
    #[derive(Default)]
    struct Acting(std::sync::Mutex<Vec<(String, serde_json::Value)>>);

    #[async_trait]
    impl Connector for Acting {
        fn id(&self) -> &str {
            "github"
        }
        async fn run(&self, ctx: ConnectorContext) -> anyhow::Result<()> {
            ctx.emit(IncomingEvent {
                source: "github".into(),
                external_id: "7".into(),
                kind: "github.ci_failed".into(),
                title: "CI failed: outage in checkout".into(),
                context: Some("acme/shop".into()),
                occurred_at: Utc::now(),
                metadata: serde_json::json!({ "repo": "acme/shop", "number": 7 }),
                ..Default::default()
            })
            .await?;
            ctx.cancelled().await;
            Ok(())
        }
        async fn act(
            &self,
            action: &str,
            input: &serde_json::Value,
        ) -> anyhow::Result<serde_json::Value> {
            self.0.lock().unwrap().push((action.into(), input.clone()));
            Ok(serde_json::json!({ "url": "https://github.com/acme/shop/pull/7#c1" }))
        }
    }

    #[tokio::test]
    async fn workflows_chain_actions_and_stop_loops() {
        use crate::workflow::{Condition, Op, RunStatus, Step, StepKind, Trigger};
        let step = |id: &str, kind: StepKind| Step {
            id: id.into(),
            label: String::new(),
            when: vec![],
            continue_on_error: false,
            kind,
        };
        let input = |pairs: &[(&str, &str)]| {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), serde_json::Value::String(v.to_string())))
                .collect()
        };
        let store = Arc::new(Store::in_memory().unwrap());
        let now = Utc::now();
        // On CI failures: comment, then a task linking the comment.
        let triage = store
            .workflow_save(
                &Workflow {
                    name: "Triage CI".into(),
                    enabled: true,
                    trigger: Trigger::Item {
                        changes: vec![Change::Ingested],
                        sources: vec!["github".into()],
                        kinds: vec!["github.ci_*".into()],
                        when: vec![],
                    },
                    steps: vec![
                        step(
                            "outage",
                            StepKind::Filter {
                                conditions: vec![Condition {
                                    field: "item.title".into(),
                                    op: Op::Contains,
                                    value: "outage".into(),
                                }],
                            },
                        ),
                        step(
                            "comment",
                            StepKind::Action {
                                connector: "github".into(),
                                action: "issue.comment".into(),
                                input: input(&[
                                    ("repo", "{{item.metadata.repo}}"),
                                    ("number", "{{item.metadata.number}}"),
                                    ("body", "Looking at {{item.title}}"),
                                ]),
                            },
                        ),
                        step(
                            "task",
                            StepKind::Action {
                                connector: "tab".into(),
                                action: "task.create".into(),
                                input: input(&[
                                    ("title", "Follow up"),
                                    ("url", "{{steps.comment.url}}"),
                                ]),
                            },
                        ),
                    ],
                    ..Default::default()
                },
                "user",
                now,
            )
            .unwrap();
        // Echoes every new tab task as another task: a loop on its own.
        let echo = store
            .workflow_save(
                &Workflow {
                    name: "Echo".into(),
                    enabled: true,
                    trigger: Trigger::Item {
                        changes: vec![Change::Ingested],
                        sources: vec!["tab".into()],
                        kinds: vec![],
                        when: vec![],
                    },
                    steps: vec![step(
                        "again",
                        StepKind::Action {
                            connector: "tab".into(),
                            action: "task.create".into(),
                            input: input(&[("title", "echo {{item.title}}")]),
                        },
                    )],
                    ..Default::default()
                },
                "user",
                now,
            )
            .unwrap();

        let (events, _) = broadcast::channel(256);
        let acting = Arc::new(Acting::default());
        let infos = connectors::registry()
            .into_iter()
            .map(|f| ConnectorInfo {
                descriptor: f.descriptor(),
                enabled: true,
                status: ConnectorStatus::Starting,
            })
            .collect();
        let handle = Engine {
            gateway: Arc::new(Gateway::new(store.clone())),
            store: store.clone(),
            ranker: Arc::new(Pipeline {
                rules: RankingRules::default(),
                profile: Default::default(),
                ai: None,
            }),
            connectors: vec![acting.clone(), Arc::new(crate::connectors::TabConnector)],
            infos,
            events,
            leases: crate::leases::InMemory::new("test"),
        }
        .start();

        let titles = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let echo_runs = store.workflow_runs(Some(&echo.id), 10).unwrap();
                if echo_runs.iter().any(|r| r.status == RunStatus::LoopStopped) {
                    let mut t: Vec<String> = store
                        .list(&[ItemState::Open])
                        .unwrap()
                        .into_iter()
                        .filter(|i| i.event.source == "tab")
                        .map(|i| i.event.title)
                        .collect();
                    t.sort();
                    return t;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the echo loop was stopped");
        assert_eq!(titles, ["Follow up", "echo Follow up"]);

        let calls = acting.0.lock().unwrap().clone();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "issue.comment");
        assert_eq!(calls[0].1["number"], 7, "a lone template keeps its type");
        assert_eq!(
            calls[0].1["body"],
            "Looking at CI failed: outage in checkout"
        );

        let run = &store.workflow_runs(Some(&triage.id), 10).unwrap()[0];
        assert_eq!(run.status, RunStatus::Succeeded);
        let follow = store
            .list(&[ItemState::Open])
            .unwrap()
            .into_iter()
            .find(|i| i.event.title == "Follow up")
            .unwrap();
        assert_eq!(
            follow.event.url.as_deref(),
            Some("https://github.com/acme/shop/pull/7#c1")
        );
        assert_eq!(
            follow.event.metadata["workflow_chain"],
            serde_json::json!([triage.id])
        );

        // Both items' timelines say what happened and who did it.
        let ci = store
            .list(&[ItemState::Open])
            .unwrap()
            .into_iter()
            .find(|i| i.event.source == "github")
            .unwrap();
        let timeline = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let t = store.activity_for(&ci.id, 50).unwrap();
                if t.iter().any(|a| a.kind == "workflow_run") {
                    return t;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the run is on the CI item's timeline");
        let kinds: Vec<&str> = timeline.iter().map(|a| a.kind.as_str()).collect();
        assert_eq!(kinds, ["arrived", "workflow_run"]);
        assert_eq!(timeline[1].title, "Workflow \"Triage CI\" ran");
        let created = &store.activity_for(&follow.id, 50).unwrap()[0];
        assert_eq!(
            (created.kind.as_str(), created.by.as_deref()),
            ("arrived", Some("workflow:Triage CI"))
        );

        // A dry run on the same item touches nothing.
        let dry = handle
            .run_workflow(
                store.workflow_get(&triage.id).unwrap().unwrap(),
                Some(&ci.id),
                true,
            )
            .await
            .unwrap();
        assert!(dry.dry_run && dry.status == RunStatus::Succeeded);
        assert_eq!(
            dry.steps[1].status,
            crate::workflow::runtime::StepStatus::Dry
        );
        assert_eq!(
            dry.steps[1].input["body"],
            "Looking at CI failed: outage in checkout"
        );
        assert_eq!(acting.0.lock().unwrap().len(), 1);
        handle.shutdown();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn command_steps_run_only_when_allowed() {
        use crate::workflow::runtime::StepStatus;
        use crate::workflow::{RunStatus, Step, StepKind, Trigger};
        let store = Arc::new(Store::in_memory().unwrap());
        let (events, _) = broadcast::channel(64);
        let handle = Engine {
            gateway: Arc::new(Gateway::new(store.clone())),
            store: store.clone(),
            ranker: Arc::new(Pipeline {
                rules: RankingRules::default(),
                profile: Default::default(),
                ai: None,
            }),
            connectors: vec![Arc::new(crate::connectors::TabConnector)],
            infos: Vec::new(),
            events,
            leases: crate::leases::InMemory::new("test"),
        }
        .start();
        let step = |id: &str, kind: StepKind| Step {
            id: id.into(),
            label: String::new(),
            when: vec![],
            continue_on_error: false,
            kind,
        };
        let mut w = Workflow {
            name: "Build".into(),
            trigger: Trigger::Manual,
            steps: vec![
                step(
                    "run",
                    StepKind::Command {
                        program: "sh".into(),
                        args: vec![
                            "-c".into(),
                            "echo \"$1\" > out.txt; cat out.txt".into(),
                            "sh".into(),
                            "{{workflow.name}}; rm -rf /".into(),
                        ],
                        folder: String::new(),
                        network: vec![],
                        env: Default::default(),
                        image: String::new(),
                        timeout_secs: 30,
                    },
                ),
                step(
                    "after",
                    StepKind::Set {
                        values: [("said".to_string(), "{{steps.run.stdout}}".into())]
                            .into_iter()
                            .collect(),
                    },
                ),
            ],
            ..Default::default()
        };
        w = store.workflow_save(&w, "user", Utc::now()).unwrap();

        let blocked = handle.run_workflow(w.clone(), None, false).await.unwrap();
        assert_eq!(blocked.status, RunStatus::Blocked);
        assert_eq!(blocked.steps[0].status, StepStatus::Blocked);

        let dry = handle.run_workflow(w.clone(), None, true).await.unwrap();
        assert_eq!(dry.steps[0].status, StepStatus::Dry);

        w.allow_commands = true;
        let w = store.workflow_save(&w, "user", Utc::now()).unwrap();
        let ran = handle.run_workflow(w, None, false).await.unwrap();
        assert_eq!(ran.status, RunStatus::Succeeded, "{:?}", ran.error);
        // A templated argument stays one argument: it's echoed, not run.
        assert_eq!(ran.steps[0].output["stdout"], "Build; rm -rf /\n");
        assert_eq!(ran.steps[0].output["exit_code"], 0);
        assert_eq!(ran.steps[1].output["said"], "Build; rm -rf /\n");
        handle.shutdown();
    }
}
