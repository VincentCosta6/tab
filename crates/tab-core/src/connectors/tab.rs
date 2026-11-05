//! tab's own connector: tasks and schedules created in tab itself, by the
//! person ("New" in Work) or by an agent through the MCP server.
//!
//! It dogfoods the connector framework: tasks are ordinary queue items with
//! source `tab`, so they get the same ranking, priority, snoozing, context
//! and knowledge-graph links as anything from Slack or GitHub. A task is
//! what an agent session hangs off.
//!
//! Tasks are written by [`crate::EngineHandle::create_task`], not polled, so
//! `run` only reports that the connector is up.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use crate::config::ConnectorConfig;
use crate::connector::{Connector, ConnectorContext, ConnectorFactory};
use crate::gateway::Gateway;
use crate::model::{ConnectorDescriptor, KindInfo, Tone};

pub struct TabFactory;

pub const SOURCE: &str = "tab";

impl ConnectorFactory for TabFactory {
    fn descriptor(&self) -> ConnectorDescriptor {
        ConnectorDescriptor {
            id: SOURCE.into(),
            name: "tab".into(),
            icon: "tab".into(),
            description: "tab's own tasks and schedules. Anything you or an agent asks tab to do becomes a task here, with a priority, an optional due time, and its own agent session.".into(),
            kinds: vec![
                KindInfo::new("tab.task", "Task", "check-square", Tone::Accent),
                KindInfo::new("tab.scheduled", "Scheduled", "clock", Tone::Warning),
                KindInfo::new("agent.needs_input", "Agent needs input", "bot", Tone::Warning),
                KindInfo::new("agent.needs_review", "Agent ready for review", "bot", Tone::Accent),
            ],
            settings: vec![],
            available: true,
            rate_limits: vec![],
            category: "built_in".into(),
            features: vec![
                "Tasks with priority (low, normal, high, urgent) and due times".into(),
                "Scheduled tasks wake up in the queue when they are due".into(),
                "Every task can own an agent session".into(),
                "Agents create and update tasks through tab's MCP server".into(),
            ],
            builtin: true,
        }
    }

    fn build(
        &self,
        _: &ConnectorConfig,
        _: Arc<Gateway>,
    ) -> Option<anyhow::Result<Arc<dyn Connector>>> {
        Some(Ok(Arc::new(TabConnector)))
    }
}

pub struct TabConnector;

#[async_trait]
impl Connector for TabConnector {
    fn id(&self) -> &str {
        SOURCE
    }

    async fn run(&self, ctx: ConnectorContext) -> anyhow::Result<()> {
        ctx.synced().await;
        ctx.cancelled().await;
        Ok(())
    }

    fn preview_ttl(&self) -> Duration {
        // Nothing remote to refresh.
        Duration::from_secs(365 * 24 * 3600)
    }
}
