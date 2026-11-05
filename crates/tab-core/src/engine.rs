//! The engine wires connectors, the store, the ranker and subscribers together.
//!
//! Every state change is published as an [`EngineEvent`] with a [`Change`]
//! describing what happened. The desktop UI subscribes to these today; the
//! automations runtime and the MCP server subscribe to the same stream later,
//! so "when an item is snoozed, do X" needs no new plumbing.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

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
    Approval, ConnectorInfo, ConnectorStatus, IncomingEvent, ItemState, NewTask, Preview,
    QueueItem, Session, SessionMessage,
};
use crate::ranker::{Pipeline, Ranker};
use crate::ratelimit;
use crate::store::{Store, Upsert};

/// What happened to an item. These are the triggers automations will hang off.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Change {
    Ingested,
    Updated,
    Ranked,
    Snoozed,
    Woke,
    Done,
    Reopened,
}

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
}

impl Engine {
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
        Self {
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
    connectors: Arc<HashMap<String, Arc<dyn Connector>>>,
    infos: Arc<Mutex<Vec<ConnectorInfo>>>,
    cancel: CancellationToken,
}

struct Core {
    store: Arc<Store>,
    gateway: Arc<Gateway>,
    ranker: Arc<dyn Ranker>,
    events: broadcast::Sender<EngineEvent>,
    rank_slots: Semaphore,
}

impl Core {
    fn publish(&self, change: Change, item: QueueItem) {
        let _ = self.events.send(EngineEvent::ItemChanged { change, item });
    }

    async fn rank(self: Arc<Self>, item: QueueItem) {
        let Ok(_slot) = self.rank_slots.acquire().await else {
            return;
        };
        match self.ranker.rank(&item).await {
            Ok(ranking) => match self.store.set_ranking(&item.id, &ranking) {
                Ok(Some(item)) => self.publish(Change::Ranked, item),
                Ok(None) => {}
                Err(e) => tracing::error!(error = %e, "saving ranking failed"),
            },
            Err(e) => {
                tracing::warn!(item = %item.id, error = %e, "ranking failed; item stays unranked")
            }
        }
    }

    /// Stores an event and ranks it. Shared by connectors and tab's own tasks.
    fn ingest(self: &Arc<Self>, e: &IncomingEvent) -> anyhow::Result<Option<QueueItem>> {
        Ok(match self.store.upsert(e, Utc::now())? {
            Upsert::Inserted(item) => {
                self.publish(Change::Ingested, item.clone());
                tokio::spawn(self.clone().rank(item.clone()));
                Some(item)
            }
            Upsert::Updated(item) => {
                // Keep showing the old preview; the refresher replaces it.
                self.store.preview_mark_dirty(&item.id)?;
                self.publish(Change::Updated, item.clone());
                tokio::spawn(self.clone().rank(item.clone()));
                Some(item)
            }
            Upsert::Unchanged => None,
        })
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

impl Engine {
    /// Spawns connectors, the ingest loop and the timers on the current Tokio runtime.
    pub fn start(self) -> EngineHandle {
        let cancel = CancellationToken::new();
        let (tx, mut rx) = mpsc::channel::<EngineMsg>(256);
        let core = Arc::new(Core {
            store: self.store,
            gateway: self.gateway,
            ranker: self.ranker,
            events: self.events,
            rank_slots: Semaphore::new(MAX_CONCURRENT_RANKS),
        });
        let infos = Arc::new(Mutex::new(self.infos));

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

        let (timer, timer_cancel) = (core.clone(), cancel.clone());
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(30));
            let mut n = 0u64;
            loop {
                tokio::select! {
                    _ = tick.tick() => {}
                    _ = timer_cancel.cancelled() => break,
                }
                match timer.store.wake_due(Utc::now()) {
                    Ok(items) => items
                        .into_iter()
                        .for_each(|i| timer.publish(Change::Woke, i)),
                    Err(e) => tracing::error!(error = %e, "waking snoozed items failed"),
                }
                if n.is_multiple_of(120)
                    && let Err(e) = timer.store.prune(Utc::now(), chrono::Duration::days(30))
                {
                    tracing::error!(error = %e, "pruning failed");
                }
                n += 1;
            }
        });

        let connectors: Arc<HashMap<String, Arc<dyn Connector>>> = Arc::new(
            self.connectors
                .into_iter()
                .map(|c| (c.id().to_string(), c))
                .collect(),
        );

        // Background preview refresher. Runs every call at background
        // priority, so the gateway defers it whenever a connector's budget
        // is tight or it has been bursting; interactive opens always win.
        let (refresh, refresh_conns, refresh_cancel) =
            (core.clone(), connectors.clone(), cancel.clone());
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
                    let Some(c) = refresh_conns.get(&item.event.source) else {
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
            connectors,
            infos,
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
        self.infos.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn changed(&self, change: Change, item: Option<QueueItem>) -> Option<QueueItem> {
        if let Some(i) = &item {
            self.core.publish(change, i.clone());
        }
        item
    }

    /// Marks an item done and lets its connector react (e.g. mark the GitHub thread read).
    pub async fn mark_done(&self, id: &str) -> anyhow::Result<Option<QueueItem>> {
        let item = self.changed(
            Change::Done,
            self.core
                .store
                .set_state(id, ItemState::Done, None, Utc::now())?,
        );
        if let Some(i) = &item
            && let Some(c) = self.connectors.get(&i.event.source)
            && let Err(e) = c.on_done(i).await
        {
            tracing::warn!(connector = c.id(), error = %e, "on_done failed");
        }
        Ok(item)
    }

    pub fn snooze(&self, id: &str, until: DateTime<Utc>) -> anyhow::Result<Option<QueueItem>> {
        Ok(self.changed(
            Change::Snoozed,
            self.core
                .store
                .set_state(id, ItemState::Snoozed, Some(until), Utc::now())?,
        ))
    }

    pub fn reopen(&self, id: &str) -> anyhow::Result<Option<QueueItem>> {
        Ok(self.changed(
            Change::Reopened,
            self.core
                .store
                .set_state(id, ItemState::Open, None, Utc::now())?,
        ))
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
        let Some(c) = self.connectors.get(&item.event.source).cloned() else {
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

    /// Creates a task in tab's own connector. A due time makes it a
    /// scheduled task that sleeps until then.
    pub fn create_task(&self, task: NewTask) -> anyhow::Result<QueueItem> {
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
            url: task.url,
            actor: Some(
                if task.created_by == "agent" {
                    "agent"
                } else {
                    "you"
                }
                .into(),
            ),
            context: Some("tab".into()),
            occurred_at: now,
            metadata,
            ..Default::default()
        };
        let item = self
            .core
            .ingest(&event)?
            .ok_or_else(|| anyhow::anyhow!("task was not stored"))?;
        if scheduled && let Some(due) = task.due_at {
            return Ok(self.snooze(&item.id, due)?.unwrap_or(item));
        }
        Ok(item)
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
        assert!(
            !handle
                .store()
                .related_items(&review.id, 5)
                .unwrap()
                .is_empty(),
            "shares acme/payments"
        );
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
}
