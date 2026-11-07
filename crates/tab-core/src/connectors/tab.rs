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
use crate::model::{
    ActionField, ActionInfo, ConnectorDescriptor, Effect, FieldType, KindInfo, SettingField,
    SettingKind, Tone,
};

pub struct TabFactory;

pub const SOURCE: &str = tab_domain::model::TAB_SOURCE;

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
            settings: vec![
                SettingField {
                    key: "semantic_search".into(),
                    label: "Search by meaning".into(),
                    kind: SettingKind::Toggle,
                    help: Some("Finds things by what they're about, not just exact words, in the search box and for agents. Downloads a small model once and indexes your items on this computer; nothing leaves it. Turning it off deletes the model and index.".into()),
                    required: false,
                },
                SettingField {
                    key: "semantic_model_large".into(),
                    label: "Use the larger model".into(),
                    kind: SettingKind::Toggle,
                    help: Some("Better matches. A 130 MB download instead of 30 MB, about 35 MB of memory instead of 8 MB, and about 2 KB per item instead of 1 KB.".into()),
                    required: false,
                },
            ],
            available: true,
            rate_limits: vec![],
            category: "built_in".into(),
            features: vec![
                "Tasks with priority (low, normal, high, urgent) and due times".into(),
                "Scheduled tasks wake up in the queue when they are due".into(),
                "Every task can own an agent session".into(),
                "Agents create and update tasks through tab's MCP server".into(),
                "Optional search by meaning, with a local model".into(),
            ],
            builtin: true,
            actions: actions(),
            clis: Vec::new(),
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

/// tab's own actions. They change the queue, never anything upstream (except
/// Done, which lets the item's connector react as it would to a click), and
/// the workflow runtime runs them itself, since they need the engine.
pub fn actions() -> Vec<ActionInfo> {
    let item = || {
        ActionField::new("item_id", "Item", FieldType::Text)
            .required()
            .help("Defaults to the item that started the run.")
            .default("{{item.id}}")
    };
    vec![
        ActionInfo::new("task.create", "Create a task", "check-square", Effect::Write,
            "Adds a task to your queue, optionally scheduled for later.")
            .inputs(vec![
                ActionField::new("title", "Title", FieldType::Text).required(),
                ActionField::new("body", "Notes", FieldType::LongText),
                ActionField::new("priority", "Priority", FieldType::Select)
                    .options(&["low", "normal", "high", "urgent"])
                    .default("normal"),
                ActionField::new("due_in_minutes", "Due in (minutes)", FieldType::Number)
                    .help("Leave empty for now. Scheduled tasks sleep until they're due."),
                ActionField::new("url", "Link", FieldType::Text).default("{{item.url}}"),
                ActionField::new("project", "Project", FieldType::Text).help("Project name. Optional."),
            ])
            .outputs(vec![ActionField::new("item_id", "New task", FieldType::Text)]),
        ActionInfo::new("item.snooze", "Snooze", "clock", Effect::Write,
            "Hides an item until later. It comes back on its own.")
            .inputs(vec![
                item(),
                ActionField::new("minutes", "For (minutes)", FieldType::Number).required().default("240"),
            ]),
        ActionInfo::new("item.done", "Mark done", "check", Effect::Write,
            "Marks an item done. Its connection reacts as it does when you click Done (GitHub marks the notification read, email marks the message read).")
            .inputs(vec![item()]),
        ActionInfo::new("item.reopen", "Reopen", "undo", Effect::Write, "Puts a done or snoozed item back in the queue.")
            .inputs(vec![item()]),
        ActionInfo::new("item.set_project", "Put in project", "folder", Effect::Write,
            "Files an item under one of your projects.")
            .inputs(vec![
                item(),
                ActionField::new("project", "Project", FieldType::Text).required().help("Project name, or empty to take it out."),
            ]),
    ]
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
