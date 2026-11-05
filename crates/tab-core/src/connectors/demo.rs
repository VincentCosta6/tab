//! Emits realistic sample work from every kind of source so the app is
//! useful to look at before any real connector is set up.

use std::time::Duration;

use async_trait::async_trait;
use chrono::{Duration as ChronoDuration, Utc};
use serde_json::json;

use std::sync::Arc;

use crate::config::ConnectorConfig;
use crate::connector::{Connector, ConnectorContext, ConnectorFactory};
use crate::gateway::Gateway;
use crate::model::{Block, ConnectorDescriptor, EntityRef, Field, IncomingEvent, QueueItem, Tone};

pub struct DemoFactory;

impl ConnectorFactory for DemoFactory {
    fn descriptor(&self) -> ConnectorDescriptor {
        ConnectorDescriptor {
            id: "demo".into(),
            name: "Sample data".into(),
            icon: "sparkles".into(),
            description: "Realistic sample work from every kind of source, so you can try tab before connecting anything.".into(),
            kinds: vec![],
            settings: vec![],
            available: true,
            rate_limits: vec![],
            category: "built_in".into(),
            features: vec![],
            builtin: false,
        }
    }

    fn build(
        &self,
        _: &ConnectorConfig,
        _: Arc<Gateway>,
    ) -> Option<anyhow::Result<Arc<dyn Connector>>> {
        Some(Ok(Arc::new(DemoConnector::default())))
    }
}

pub struct DemoConnector {
    pub interval: Duration,
}

impl Default for DemoConnector {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(40),
        }
    }
}

struct Sample {
    kind: &'static str,
    title: &'static str,
    body: &'static str,
    actor: &'static str,
    context: &'static str,
    url: &'static str,
    minutes_ago: i64,
}

const SAMPLES: &[Sample] = &[
    Sample {
        kind: "github.review_requested",
        title: "Review requested: Add idempotency keys to payment webhooks (#482)",
        body: "Dana requested your review. 14 files changed, +612 −203. \"Need this merged before Thursday's release cut, happy to pair if it's easier.\"",
        actor: "dana-k",
        context: "acme/payments",
        url: "https://github.com/acme/payments/pull/482",
        minutes_ago: 35,
    },
    Sample {
        kind: "slack.mention",
        title: "@you in #incident-checkout: are you around? latency on /checkout is 4x",
        body: "p99 on /checkout jumped from 400ms to 1.7s after the 14:10 deploy. Rolling back now but need someone who knows the cart cache to confirm.",
        actor: "Priya (on-call)",
        context: "#incident-checkout",
        url: "https://slack.com/app_redirect?channel=incident-checkout",
        minutes_ago: 4,
    },
    Sample {
        kind: "agent.needs_input",
        title: "Agent session blocked: which migration strategy for the orders table?",
        body: "Working on \"split orders.status into enum\". Two options: (a) backfill in place with a lock, ~3 min downtime, (b) dual-write for one release. Waiting on your choice before continuing.",
        actor: "Claude · orders-refactor",
        context: "acme/orders",
        url: "",
        minutes_ago: 12,
    },
    Sample {
        kind: "github.ci_failed",
        title: "CI failed on your PR: Migrate auth middleware to tower (#1290)",
        body: "integration-tests (ubuntu-latest) failed: test_refresh_token_rotation timed out after 30s.",
        actor: "github-actions",
        context: "acme/api",
        url: "https://github.com/acme/api/pull/1290/checks",
        minutes_ago: 50,
    },
    Sample {
        kind: "meeting.action_item",
        title: "From \"Q4 planning sync\": you own the rate-limit design doc",
        body: "Zoom AI summary: \"Vincent to draft the rate-limiting design doc and circulate by Friday. Marco to provide current traffic numbers.\"",
        actor: "Zoom AI Companion",
        context: "Q4 planning sync",
        url: "",
        minutes_ago: 95,
    },
    Sample {
        kind: "github.comment",
        title: "3 new comments on Refactor queue worker retries (#477)",
        body: "Marco: \"Should the backoff cap be configurable? 5 min feels long for the email worker.\"",
        actor: "marco-r",
        context: "acme/payments",
        url: "https://github.com/acme/payments/pull/477",
        minutes_ago: 140,
    },
    Sample {
        kind: "email.direct",
        title: "Re: SOC 2 evidence for access reviews",
        body: "Hi, could you send the screenshots of the quarterly access review for the prod database by end of next week? Thanks, Alex (Compliance)",
        actor: "alex@acme.com",
        context: "Inbox",
        url: "",
        minutes_ago: 300,
    },
    Sample {
        kind: "agent.needs_review",
        title: "Agent finished: Add pagination to /v2/invoices — ready for review",
        body: "Opened PR #491. All tests pass. Summary: cursor-based pagination, default page size 50, backwards compatible.",
        actor: "Claude · invoices-pagination",
        context: "acme/payments",
        url: "https://github.com/acme/payments/pull/491",
        minutes_ago: 22,
    },
    Sample {
        kind: "slack.channel",
        title: "#eng-announcements: Reminder, office closed Monday",
        body: "Friendly reminder that the office is closed Monday for the holiday. On-call rotation is unchanged.",
        actor: "People Team",
        context: "#eng-announcements",
        url: "",
        minutes_ago: 400,
    },
    Sample {
        kind: "email.newsletter",
        title: "This week in Rust #612",
        body: "Weekly digest. Unsubscribe anytime.",
        actor: "TWiR",
        context: "Inbox",
        url: "",
        minutes_ago: 600,
    },
    Sample {
        kind: "slack.dm",
        title: "DM from Sam: quick q on the feature flag for the new onboarding flow",
        body: "Is it safe to flip onboarding_v2 to 50% today, or are we still waiting on the analytics fix?",
        actor: "Sam (PM)",
        context: "Direct message",
        url: "",
        minutes_ago: 18,
    },
];

/// Entities for a sample: who it's from and where it happened, keyed the way
/// real connectors would key them so the graph links up.
fn entities(s: &Sample) -> Vec<EntityRef> {
    let app = s.kind.split('.').next().unwrap_or("");
    let mut v = vec![EntityRef::new(
        "person",
        format!("{app}:{}", s.actor),
        s.actor,
        "from",
    )];
    if s.context.contains('/') {
        v.push(EntityRef::new(
            "repo",
            format!("github:{}", s.context),
            s.context,
            "in",
        ));
    } else if s.context.starts_with('#') {
        v.push(EntityRef::new(
            "channel",
            format!("slack:{}", s.context),
            s.context,
            "in",
        ));
    } else if app == "meeting" {
        v.push(EntityRef::new(
            "meeting",
            format!("zoom:{}", s.context),
            s.context,
            "in",
        ));
    }
    if let Some(n) = s
        .url
        .split("/pull/")
        .nth(1)
        .and_then(|r| r.split('/').next())
    {
        v.push(EntityRef::new(
            "pull_request",
            format!("github:{}#{n}", s.context),
            format!("{}#{n}", s.context),
            "about",
        ));
    }
    if s.kind.starts_with("agent.") {
        let session = s.actor.split(" · ").nth(1).unwrap_or(s.actor);
        v.push(EntityRef::new(
            "agent_session",
            format!("agent:{session}"),
            session,
            "from",
        ));
    }
    v
}

fn blocks(s: &Sample) -> Vec<Block> {
    let mut v = vec![Block::Quote {
        text: s.body.into(),
        author: Some(s.actor.into()),
    }];
    if s.kind == "agent.needs_input" {
        v.push(Block::Choices {
            prompt: "Which migration strategy?".into(),
            options: vec![
                "Backfill in place (~3 min downtime)".into(),
                "Dual-write for one release".into(),
            ],
        });
    }
    v
}

fn to_event(s: &Sample, external_id: String, minutes_ago: i64) -> IncomingEvent {
    IncomingEvent {
        source: "demo".into(),
        external_id,
        kind: s.kind.into(),
        title: s.title.into(),
        body: Some(s.body.into()),
        url: (!s.url.is_empty()).then(|| s.url.to_string()),
        actor: Some(s.actor.into()),
        context: Some(s.context.into()),
        occurred_at: Utc::now() - ChronoDuration::minutes(minutes_ago),
        blocks: blocks(s),
        entities: entities(s),
        metadata: json!({ "demo": true }),
    }
}

#[async_trait]
impl Connector for DemoConnector {
    fn id(&self) -> &str {
        "demo"
    }

    async fn run(&self, ctx: ConnectorContext) -> anyhow::Result<()> {
        // Seed ids are stable, so restarts don't duplicate the seed set.
        for (i, s) in SAMPLES.iter().enumerate() {
            ctx.emit(to_event(s, format!("seed-{i}"), s.minutes_ago))
                .await?;
        }
        ctx.synced().await;

        // Then trickle in fresh copies so the live-update path gets exercised.
        let mut n = 0usize;
        while ctx.sleep(self.interval).await {
            let s = &SAMPLES[n % SAMPLES.len()];
            let id = format!("live-{}-{n}", Utc::now().timestamp());
            ctx.emit(to_event(s, id, 0)).await?;
            ctx.synced().await;
            n += 7; // co-prime with the sample count, so the order looks varied
        }
        Ok(())
    }

    /// A canned preview shaped like what the GitHub connector returns.
    async fn preview(&self, item: &QueueItem) -> anyhow::Result<Vec<Block>> {
        if !item.event.kind.starts_with("github.") && item.event.kind != "agent.needs_review" {
            return Ok(Vec::new());
        }
        Ok(vec![
            Block::Stats {
                items: vec![
                    Field::new("added", "+612").tone(Tone::Success),
                    Field::new("removed", "−203").tone(Tone::Danger),
                    Field::new("files", "14"),
                    Field::new("commits", "6"),
                ],
            },
            Block::Fields {
                items: vec![
                    Field::new("State", "Open").tone(Tone::Success),
                    Field::new("Branch", "idempotency-keys → main"),
                    Field::new("Checks", "11 passed, 1 pending").tone(Tone::Warning),
                ],
            },
            Block::Text {
                text: "Adds an `Idempotency-Key` header to all webhook deliveries and stores processed keys for 24h.\n\n- Dedupes retries from the queue worker\n- Backfills keys for in-flight deliveries".into(),
                markdown: true,
            },
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_link_into_the_graph() {
        let review = SAMPLES
            .iter()
            .find(|s| s.kind == "github.review_requested")
            .unwrap();
        let keys: Vec<_> = entities(review).into_iter().map(|e| e.key).collect();
        assert_eq!(
            keys,
            [
                "github:dana-k",
                "github:acme/payments",
                "github:acme/payments#482"
            ]
        );
    }
}
