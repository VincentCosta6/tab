//! The AI layer on a decision model (Jev, Luna) instead of Claude Haiku.
//!
//! Each item gets one request with three questions
//! ([`tab_domain::decision::ranking::questions`]): how urgent it is on the
//! same four-level scale the Haiku prompt uses, whether it's addressed to the
//! person, and whether someone is blocked on them. The answers become a
//! nudge of at most ±[`MAX_AI_ADJUSTMENT`], scaled by the model's
//! confidence, and a reason built from them (decision models write no text).
//!
//! Determinism: cached under a hash of provider, model, question version and
//! the state, like the Haiku layer. Same inputs, same answer, no call.

use chrono::Utc;
use sha2::{Digest, Sha256};
use tab_domain::decision::{self, ranking};

use super::{AiVerdict, MAX_AI_ADJUSTMENT, RuleResult};
use crate::config::BudgetConfig;
use crate::decisions::Decider;
use crate::model::{QueueItem, UserProfile};
use crate::store::Store;
use std::sync::Arc;

pub struct DecisionLayer {
    decider: Decider,
    store: Arc<Store>,
    instructions: String,
    budget: BudgetConfig,
}

impl DecisionLayer {
    pub fn new(
        decider: Decider,
        store: Arc<Store>,
        instructions: String,
        budget: BudgetConfig,
    ) -> Self {
        Self {
            decider,
            store,
            instructions,
            budget,
        }
    }

    /// e.g. `jev-1.14`, for `ranked_by`.
    pub fn model(&self) -> &str {
        let id = &self.decider.model().id;
        id.rsplit('/').next().unwrap_or(id)
    }

    pub fn provider(&self) -> &str {
        &self.decider.provider().id
    }

    pub async fn adjust(
        &self,
        item: &QueueItem,
        profile: &UserProfile,
        base: &RuleResult,
    ) -> anyhow::Result<AiVerdict> {
        let (person, item_json, rules) = super::ai::facts(item, profile, base);
        let state = decision::ranking_state(person, &self.instructions, rules, item_json);
        let key = {
            let d = Sha256::digest(format!(
                "decision v{}\n{}\n{}\n{state}",
                ranking::QUESTIONS_VERSION,
                self.decider.provider().id,
                self.decider.model().id,
            ));
            d.iter().map(|b| format!("{b:02x}")).collect::<String>()
        };
        if let Some(hit) = self.store.ai_cache_get(&key)? {
            return Ok(serde_json::from_str(&hit)?);
        }
        if let Some(why) = self.budget.blocked(&self.store.usage_summary(Utc::now())?) {
            anyhow::bail!(why);
        }
        let answers = self
            .decider
            .decide(&state, &ranking::questions(), "ranker")
            .await?;
        let n = ranking::nudge(base.score, &answers, MAX_AI_ADJUSTMENT)
            .ok_or_else(|| anyhow::anyhow!("{} gave no urgency", self.model()))?;
        let v = AiVerdict {
            adjustment: n.adjustment,
            reason: n.reason,
        };
        self.store
            .ai_cache_put(&key, &self.decider.model().id, &serde_json::to_string(&v)?)?;
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::config::DecisionConfig;
    use crate::decisions::tests::serve;
    use crate::gateway::Gateway;
    use crate::model::IncomingEvent;
    use crate::ranker::{Pipeline, Ranker, RankingRules};
    use crate::store::Upsert;

    fn pipeline(base: &str, store: Arc<Store>) -> Pipeline {
        let gw = Arc::new(Gateway::new(store.clone()));
        let cfg = DecisionConfig {
            provider: "openrouter".into(),
            api_keys: [("openrouter".to_string(), "k".to_string())].into(),
            base_url: Some(base.into()),
            ..Default::default()
        };
        let d = Decider::from_config(&cfg, gw, store.clone()).unwrap();
        Pipeline {
            rules: RankingRules::default(),
            profile: Default::default(),
            ai: None,
            decision: Some(DecisionLayer::new(
                d,
                store,
                "On-call pages are urgent.".into(),
                BudgetConfig::default(),
            )),
        }
    }

    fn item(store: &Store) -> QueueItem {
        let e = IncomingEvent {
            source: "slack".into(),
            external_id: "1".into(),
            kind: "slack.mention".into(),
            title: "@you prod checkout is down".into(),
            occurred_at: Utc::now(),
            ..Default::default()
        };
        let Upsert::Inserted(item) = store.upsert(&e, Utc::now()).unwrap() else {
            unreachable!()
        };
        item
    }

    #[tokio::test]
    async fn ranks_with_the_decision_model_and_caches_it() {
        let answer = json!({
            "answers": {"urgency": {"type": "score", "score": 3.0, "confidence": 1.0},
                        "blocking": {"type": "noul", "noul": 0.9}},
            "usage": {"input_tokens": 500, "output_tokens": 0, "cost": 0.00002}
        });
        // One canned answer: a second request would find no server.
        let (base, seen) = serve(vec![answer.to_string()]);
        let store = Arc::new(Store::in_memory().unwrap());
        let p = pipeline(&base, store.clone());
        let it = item(&store);
        let r = p.rank(&it).await.unwrap();
        assert_eq!(r.ai_adjustment, Some(MAX_AI_ADJUSTMENT));
        assert_eq!(r.ranked_by, "rules+jev-1.14");
        assert_eq!(
            r.reason,
            "Likely needs you right now (100% sure), someone is waiting on you."
        );
        let again = p.rank(&it).await.unwrap();
        assert_eq!(again, r);
        assert_eq!(seen.lock().unwrap().len(), 1);
        let req = &seen.lock().unwrap()[0];
        assert!(req.starts_with("POST /api/v1/systemone"));
        assert!(req.contains("On-call pages are urgent."));
    }

    #[tokio::test]
    async fn a_failed_call_leaves_the_rules_score() {
        let store = Arc::new(Store::in_memory().unwrap());
        let p = pipeline("http://127.0.0.1:1", store.clone());
        let r = p.rank(&item(&store)).await.unwrap();
        assert_eq!(r.ranked_by, "rules");
        assert_eq!(r.ai_adjustment, None);
        assert_eq!(r.score, r.base_score);
    }
}
