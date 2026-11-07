//! Ranking is two layers:
//!
//! 1. [`RankingRules`]: deterministic, configurable rules. Same item and same
//!    rules always give the same score. This is the floor the queue relies on.
//! 2. [`AiLayer`] (optional): Claude Haiku reads the item, the person's profile
//!    and their custom instructions, and nudges the score by a bounded amount
//!    with a one-line reason. Results are cached by a hash of every input, so
//!    re-ranking the same item with the same instructions is a cache hit and
//!    returns the identical answer.
//!
//! The final order is `score DESC, occurred_at DESC, id`, so it is total and
//! reproducible.

mod ai;

use std::sync::Arc;

use async_trait::async_trait;

pub use ai::{AiLayer, DEFAULT_MODEL, MAX_AI_ADJUSTMENT};
pub use tab_domain::rules::{KeywordRule, KindScore, RankingRules, RuleResult};

use crate::TabConfig;
use crate::gateway::Gateway;
use crate::model::{QueueItem, Ranking, Urgency, UserProfile};
use crate::store::Store;

#[async_trait]
pub trait Ranker: Send + Sync {
    async fn rank(&self, item: &QueueItem) -> anyhow::Result<Ranking>;
}

pub struct Pipeline {
    pub rules: RankingRules,
    pub profile: UserProfile,
    pub ai: Option<AiLayer>,
}

impl Pipeline {
    pub fn from_config(cfg: &TabConfig, gateway: Arc<Gateway>, store: Arc<Store>) -> Self {
        let ai = AiLayer::from_config(cfg, gateway, store);
        Self {
            rules: cfg.ranking.rules.clone(),
            profile: cfg.profile.clone(),
            ai,
        }
    }
}

#[async_trait]
impl Ranker for Pipeline {
    async fn rank(&self, item: &QueueItem) -> anyhow::Result<Ranking> {
        let base = self.rules.score(&item.event, &self.profile);
        let mut ranking = Ranking {
            score: base.score,
            base_score: base.score,
            ai_adjustment: None,
            urgency: Urgency::from_score(base.score),
            reason: base.reason.clone(),
            ranked_by: "rules".into(),
            fired: base.fired.clone(),
        };
        // Muted items are final; don't spend tokens on them.
        if base.muted {
            return Ok(ranking);
        }
        // Connectors mark mail like newsletters rules-only, so it never spends AI.
        if item.event.metadata["ai"] == serde_json::Value::Bool(false) {
            return Ok(ranking);
        }
        if let Some(ai) = &self.ai {
            match ai.adjust(item, &self.profile, &base).await {
                Ok(v) => {
                    let score = (base.score as i16 + v.adjustment).clamp(0, 100) as u8;
                    ranking.score = score;
                    ranking.ai_adjustment = Some(v.adjustment);
                    ranking.urgency = Urgency::from_score(score);
                    ranking.reason = v.reason;
                    ranking.ranked_by = format!("rules+{}", ai.model());
                }
                // The rules result stands on its own; the AI layer is a refinement.
                Err(e) => tracing::warn!(item = %item.id, error = %e, "AI layer skipped"),
            }
        }
        Ok(ranking)
    }
}
