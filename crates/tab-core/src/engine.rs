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
use crate::model::{Block, ConnectorInfo, ConnectorStatus, ItemState, QueueItem};
use crate::ranker::{Pipeline, Ranker};
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
    ItemChanged { change: Change, item: QueueItem },
    ConnectorChanged { connector: ConnectorInfo },
}

const MAX_CONCURRENT_RANKS: usize = 4;

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
    connectors: HashMap<String, Arc<dyn Connector>>,
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
                    EngineMsg::Event(e) => match ingest.store.upsert(&e, Utc::now()) {
                        Ok(Upsert::Inserted(item)) => {
                            ingest.publish(Change::Ingested, item.clone());
                            tokio::spawn(ingest.clone().rank(item));
                        }
                        Ok(Upsert::Updated(item)) => {
                            ingest.publish(Change::Updated, item.clone());
                            tokio::spawn(ingest.clone().rank(item));
                        }
                        Ok(Upsert::Unchanged) => {}
                        Err(e) => tracing::error!(error = %e, "storing event failed"),
                    },
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

        EngineHandle {
            core,
            connectors: self
                .connectors
                .into_iter()
                .map(|c| (c.id().to_string(), c))
                .collect(),
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

    /// Rich content for the detail pane, from the item's connector (cached by the gateway).
    pub async fn preview(&self, id: &str) -> anyhow::Result<Vec<Block>> {
        let Some(item) = self.core.store.get(id)? else {
            return Ok(Vec::new());
        };
        match self.connectors.get(&item.event.source) {
            Some(c) => c.preview(&item).await,
            None => Ok(Vec::new()),
        }
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
    async fn default_config_runs_sample_data_end_to_end() {
        let (events, _) = broadcast::channel(256);
        let store = Arc::new(Store::in_memory().unwrap());
        let gateway = Arc::new(Gateway::new(store.clone()));
        let engine = Engine::from_config(&TabConfig::default(), store.clone(), gateway, events);
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
        assert!(handle.preview(&review.id).await.unwrap().len() >= 2);
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
