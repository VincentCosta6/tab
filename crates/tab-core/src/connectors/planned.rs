//! Connectors that are designed but not built yet. They show in the
//! Connections tab as "coming soon" so the shape of the product is visible.

use std::sync::Arc;

use crate::config::ConnectorConfig;
use crate::connector::{Connector, ConnectorFactory};
use crate::gateway::Gateway;
use crate::model::{ConnectorDescriptor, KindInfo, Tone};

struct Planned(fn() -> ConnectorDescriptor);

impl ConnectorFactory for Planned {
    fn descriptor(&self) -> ConnectorDescriptor {
        (self.0)()
    }
    fn build(
        &self,
        _: &ConnectorConfig,
        _: Arc<Gateway>,
    ) -> Option<anyhow::Result<Arc<dyn Connector>>> {
        None
    }
}

fn d(
    id: &str,
    name: &str,
    icon: &str,
    description: &str,
    kinds: Vec<KindInfo>,
) -> ConnectorDescriptor {
    ConnectorDescriptor {
        id: id.into(),
        name: name.into(),
        icon: icon.into(),
        description: description.into(),
        kinds,
        settings: vec![],
        available: false,
    }
}

pub fn factories() -> Vec<Box<dyn ConnectorFactory>> {
    vec![
        Box::new(Planned(|| {
            d(
                "agents",
                "Agent sessions",
                "bot",
                "Claude Code, Codex, OpenCode or any CLI agent, streamed live with its terminal. Sessions that need you land in the queue.",
                vec![
                    KindInfo::new(
                        "agent.needs_input",
                        "Agent needs input",
                        "bot",
                        Tone::Warning,
                    ),
                    KindInfo::new(
                        "agent.needs_review",
                        "Agent ready for review",
                        "bot",
                        Tone::Accent,
                    ),
                ],
            )
        })),
        Box::new(Planned(|| {
            d(
                "slack",
                "Slack",
                "slack",
                "Mentions, DMs and threads you're in.",
                vec![
                    KindInfo::new("slack.mention", "Mention", "slack", Tone::Info),
                    KindInfo::new("slack.dm", "Direct message", "slack", Tone::Info),
                    KindInfo::new("slack.channel", "Channel", "slack", Tone::Neutral),
                ],
            )
        })),
        Box::new(Planned(|| {
            d(
                "gmail",
                "Gmail",
                "mail",
                "Mail addressed to you, with newsletters and bots filtered down.",
                vec![
                    KindInfo::new("email.direct", "Email", "mail", Tone::Neutral),
                    KindInfo::new("email.newsletter", "Newsletter", "mail", Tone::Neutral),
                ],
            )
        })),
        Box::new(Planned(|| {
            d(
                "zoom",
                "Zoom",
                "video",
                "Action items assigned to you from meeting AI summaries.",
                vec![KindInfo::new(
                    "meeting.action_item",
                    "Meeting action item",
                    "video",
                    Tone::Accent,
                )],
            )
        })),
        Box::new(Planned(|| {
            d(
                "jira",
                "Jira",
                "ticket",
                "Issues assigned to you, mentions, and status changes on work you own.",
                vec![],
            )
        })),
    ]
}
