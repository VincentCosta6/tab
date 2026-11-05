//! Connectors that are designed but not built yet. They show in the
//! Connections tab as "coming soon" so the shape of the product is visible.

use std::sync::Arc;

use crate::config::ConnectorConfig;
use crate::connector::{Connector, ConnectorFactory};
use crate::gateway::Gateway;
use crate::model::{ConnectorDescriptor, KindInfo, Tone};
use crate::ratelimit::RateLimitSpec;

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
        rate_limits: vec![],
        category: String::new(),
        features: vec![],
        builtin: false,
    }
}

fn with(mut d: ConnectorDescriptor, category: &str, limits: Vec<RateLimitSpec>) -> ConnectorDescriptor {
    d.category = category.into();
    d.rate_limits = limits;
    d
}

pub fn factories() -> Vec<Box<dyn ConnectorFactory>> {
    vec![
        Box::new(Planned(|| {
            with(
                d(
                    "slack",
                    "Slack",
                    "slack",
                    "Mentions, DMs and threads you're in, with who said it, where, and a short preview.",
                    vec![
                        KindInfo::new("slack.mention", "Mention", "slack", Tone::Info),
                        KindInfo::new("slack.dm", "Direct message", "slack", Tone::Info),
                        KindInfo::new("slack.channel", "Channel", "slack", Tone::Neutral),
                    ],
                ),
                "communication",
                // Web API tiers: most read methods are Tier 3 (~50/min), history is Tier 3/4.
                vec![
                    RateLimitSpec::new("slack", "default", 50, 60, 15)
                        .note("Tier 3 methods: about 50 requests per minute"),
                ],
            )
        })),
        Box::new(Planned(|| {
            with(
                d(
                    "gmail",
                    "Gmail",
                    "gmail",
                    "Mail addressed to you, with newsletters and bots filtered down.",
                    vec![
                        KindInfo::new("email.direct", "Email", "gmail", Tone::Neutral),
                        KindInfo::new("email.newsletter", "Newsletter", "gmail", Tone::Neutral),
                    ],
                ),
                "communication",
                vec![
                    RateLimitSpec::new("gmail", "default", 250, 1, 250)
                        .note("250 quota units per second per user"),
                ],
            )
        })),
        Box::new(Planned(|| {
            with(
                d(
                    "zoom",
                    "Zoom",
                    "zoom",
                    "Action items assigned to you from meeting AI summaries.",
                    vec![KindInfo::new(
                        "meeting.action_item",
                        "Meeting action item",
                        "zoom",
                        Tone::Accent,
                    )],
                ),
                "communication",
                vec![
                    RateLimitSpec::new("zoom", "default", 30, 1, 30)
                        .note("Medium APIs: 30 requests per second"),
                ],
            )
        })),
        Box::new(Planned(|| {
            with(
                d(
                    "jira",
                    "Jira",
                    "jira",
                    "Issues assigned to you, mentions, and status changes on work you own.",
                    vec![
                        KindInfo::new("jira.assigned", "Assigned issue", "jira", Tone::Accent),
                        KindInfo::new("jira.mention", "Mention", "jira", Tone::Info),
                    ],
                ),
                "work",
                vec![
                    RateLimitSpec::new("jira", "default", 100, 60, 20)
                        .note("Cost-based; tab budgets about 100 requests per minute"),
                ],
            )
        })),
    ]
}
