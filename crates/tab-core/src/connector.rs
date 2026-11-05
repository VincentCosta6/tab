//! The connector contract.
//!
//! A connector is anything that can notice work for a human: a Slack
//! workspace, a GitHub account, a mailbox, a meeting recorder, a fleet of
//! agent sessions. Each one runs as its own task, pushes [`IncomingEvent`]s
//! through its [`ConnectorContext`], and may react when the person finishes
//! an item (for example, marking the GitHub notification read).

use async_trait::async_trait;
use chrono::Utc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use std::sync::Arc;

use crate::config::ConnectorConfig;
use crate::gateway::Gateway;
use crate::model::{Block, ConnectorDescriptor, ConnectorStatus, IncomingEvent, QueueItem};

/// Describes a connector type and builds instances of it from settings.
///
/// The registry of factories is what the Connections tab lists. Adding a
/// connector means adding a factory; nothing else in the app changes.
pub trait ConnectorFactory: Send + Sync {
    fn descriptor(&self) -> ConnectorDescriptor;
    /// `None` for connectors that are described but not built yet.
    fn build(
        &self,
        cfg: &ConnectorConfig,
        gateway: Arc<Gateway>,
    ) -> Option<anyhow::Result<Arc<dyn Connector>>>;
}

#[async_trait]
pub trait Connector: Send + Sync + 'static {
    /// Stable id, also used as `IncomingEvent::source`.
    fn id(&self) -> &str;

    /// Runs until `ctx.cancelled()` fires. Returning an error marks the
    /// connector as failed in the UI; it is not restarted automatically.
    async fn run(&self, ctx: ConnectorContext) -> anyhow::Result<()>;

    /// Called after the person marks one of this connector's items done.
    async fn on_done(&self, _item: &QueueItem) -> anyhow::Result<()> {
        Ok(())
    }

    /// Richer content for the detail pane, fetched on demand. Implementations
    /// should go through the gateway with a cache TTL so previews are cheap.
    async fn preview(&self, _item: &QueueItem) -> anyhow::Result<Vec<Block>> {
        Ok(Vec::new())
    }
}

pub(crate) enum EngineMsg {
    Event(Box<IncomingEvent>),
    Status(String, ConnectorStatus),
}

#[derive(Clone)]
pub struct ConnectorContext {
    id: String,
    tx: mpsc::Sender<EngineMsg>,
    cancel: CancellationToken,
}

impl ConnectorContext {
    pub(crate) fn new(id: String, tx: mpsc::Sender<EngineMsg>, cancel: CancellationToken) -> Self {
        Self { id, tx, cancel }
    }

    /// Hands an event to the engine. Fails only when the engine is shutting down.
    pub async fn emit(&self, event: IncomingEvent) -> anyhow::Result<()> {
        self.tx
            .send(EngineMsg::Event(Box::new(event)))
            .await
            .map_err(|_| anyhow::anyhow!("engine stopped"))
    }

    pub async fn synced(&self) {
        self.set_status(ConnectorStatus::Running {
            last_sync: Some(Utc::now()),
        })
        .await;
    }

    pub async fn set_status(&self, status: ConnectorStatus) {
        let _ = self
            .tx
            .send(EngineMsg::Status(self.id.clone(), status))
            .await;
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    pub async fn cancelled(&self) {
        self.cancel.cancelled().await
    }

    /// Sleeps for `d`, returning `false` early if the engine is shutting down.
    pub async fn sleep(&self, d: std::time::Duration) -> bool {
        tokio::select! {
            _ = tokio::time::sleep(d) => true,
            _ = self.cancel.cancelled() => false,
        }
    }
}
