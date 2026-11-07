//! Decision models, as data (`decisions.json`), like agent harnesses.
//!
//! A catalog entry is a provider: where it lives, how it signs in, the
//! models it serves with their prices and capabilities, and its `dialect`,
//! the wire format one adapter here speaks. A provider that speaks a known
//! dialect is a new entry and nothing else; a new format is an entry plus
//! one adapter. Callers ask tab's own questions
//! ([`tab_domain::decision::Question`]) and get tab's own answers back, so
//! the ranker never sees a provider's format.
//!
//! Every call goes through the gateway, is priced into the usage counters,
//! and is refused once the configured budget is spent.

mod keys;
mod openai;
mod system_one;

use std::sync::{Arc, LazyLock};

use chrono::Utc;
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::Value;
pub use tab_domain::decision::{Answer, Capabilities, Opt, Question, QuestionKind};

use crate::config::DecisionConfig;
use crate::gateway::{Call, Gateway};
use crate::store::Store;

pub use keys::harness_key;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Provider {
    pub id: String,
    pub name: String,
    pub pitch: String,
    /// The wire format: `system_one` or `openai_decisions`.
    pub dialect: String,
    pub base_url: String,
    pub path: String,
    /// The environment variable that holds a key when settings don't.
    pub env: String,
    /// Where the person gets a key.
    pub key_url: String,
    /// Harnesses whose saved key for this provider tab may offer to reuse.
    #[serde(default)]
    pub key_from: Vec<String>,
    pub models: Vec<Model>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Model {
    pub id: String,
    pub label: String,
    /// `Jev`, `Luna`: the same model served by two providers shares a family.
    pub family: String,
    /// List prices, dollars per million tokens.
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
    pub capabilities: Capabilities,
}

static PROVIDERS: LazyLock<Vec<Provider>> = LazyLock::new(|| {
    serde_json::from_str::<Vec<Provider>>(include_str!("decisions.json"))
        .expect("decisions.json is valid")
        .into_iter()
        .filter(|p| dialect(&p.dialect).is_some())
        .collect()
});

/// Every provider tab has an adapter for.
pub fn providers() -> &'static [Provider] {
    &PROVIDERS
}

pub fn provider(id: &str) -> Option<&'static Provider> {
    PROVIDERS.iter().find(|p| p.id == id)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dialect {
    SystemOne,
    OpenAi,
}

fn dialect(name: &str) -> Option<Dialect> {
    match name {
        "system_one" => Some(Dialect::SystemOne),
        "openai_decisions" => Some(Dialect::OpenAi),
        _ => None,
    }
}

/// What one call cost, as the provider reported it or priced from tokens.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Usage {
    pub input_tokens: i64,
    pub output_tokens: i64,
    /// Dollars, when the provider says (OpenRouter does).
    pub cost_usd: Option<f64>,
}

/// Asks one provider's model questions. Built from config; `None` when no
/// provider is set or no key can be found.
pub struct Decider {
    gateway: Arc<Gateway>,
    store: Arc<Store>,
    provider: &'static Provider,
    model: &'static Model,
    dialect: Dialect,
    url: String,
}

impl Decider {
    pub fn from_config(
        cfg: &DecisionConfig,
        gateway: Arc<Gateway>,
        store: Arc<Store>,
    ) -> Option<Self> {
        let provider = provider(cfg.provider.trim())?;
        let model = match cfg.model.as_deref().filter(|m| !m.trim().is_empty()) {
            Some(id) => provider.models.iter().find(|m| m.id == id)?,
            None => provider.models.first()?,
        };
        let key = keys::resolve(cfg, provider)?;
        let mut h = HeaderMap::new();
        let mut v = HeaderValue::from_str(&format!("Bearer {key}")).ok()?;
        v.set_sensitive(true);
        h.insert(AUTHORIZATION, v);
        gateway.set_provider_headers(&provider.id, h);
        let base = cfg
            .base_url
            .as_deref()
            .filter(|b| !b.trim().is_empty())
            .unwrap_or(&provider.base_url);
        Some(Self {
            gateway,
            store,
            provider,
            model,
            dialect: dialect(&provider.dialect)?,
            url: format!("{}{}", base.trim_end_matches('/'), provider.path),
        })
    }

    pub fn provider(&self) -> &'static Provider {
        self.provider
    }

    pub fn model(&self) -> &'static Model {
        self.model
    }

    /// Answers every question about `state`, keyed by question id. Questions
    /// the model declined come back as [`Answer::Refused`].
    pub async fn decide(
        &self,
        state: &Value,
        questions: &[Question],
        purpose: &'static str,
    ) -> anyhow::Result<Vec<(String, Answer)>> {
        let issues = tab_domain::decision::validate(questions, &self.model.capabilities);
        if let Some(first) = issues.first() {
            anyhow::bail!("{first}");
        }
        let body = match self.dialect {
            Dialect::SystemOne => system_one::request(&self.model.id, state, questions),
            Dialect::OpenAi => openai::request(&self.model.id, state, questions),
        };
        let resp = self
            .gateway
            .send(Call::post(&self.provider.id, purpose, "decide", &self.url).json(body))
            .await?;
        let v: Value = resp.json()?;
        if resp.status >= 400 {
            let msg = v["error"]["message"]
                .as_str()
                .or_else(|| v["error"].as_str())
                .unwrap_or("request failed");
            anyhow::bail!("{} said {}: {msg}", self.provider.name, resp.status);
        }
        let (answers, usage) = match self.dialect {
            Dialect::SystemOne => system_one::response(&v, questions)?,
            Dialect::OpenAi => openai::response(&v, questions)?,
        };
        self.record(purpose, &usage);
        Ok(answers)
    }

    fn record(&self, purpose: &'static str, u: &Usage) {
        let cost = u.cost_usd.unwrap_or(
            (u.input_tokens as f64 * self.model.input_per_mtok
                + u.output_tokens as f64 * self.model.output_per_mtok)
                / 1_000_000.0,
        );
        let usage = crate::store::NewUsage {
            purpose,
            agent: self.provider.id.clone(),
            model: Some(self.model.id.clone()),
            billing: "api",
            cost_usd: cost,
            input_tokens: u.input_tokens,
            output_tokens: u.output_tokens,
            ..Default::default()
        };
        let _ = self.store.usage_add(&usage, Utc::now());
    }
}

/// For the settings page: each provider, whether a key is ready, and which
/// harnesses have one tab could reuse. Never the keys themselves.
#[derive(Debug, Clone, Serialize)]
pub struct ProviderStatus {
    #[serde(flatten)]
    pub provider: Provider,
    /// A key is set in settings or the environment.
    pub has_key: bool,
    /// Harnesses that already hold a key for this provider.
    pub reusable_from: Vec<String>,
}

pub fn status(cfg: &DecisionConfig) -> Vec<ProviderStatus> {
    providers()
        .iter()
        .map(|p| ProviderStatus {
            provider: p.clone(),
            has_key: cfg
                .api_keys
                .get(&p.id)
                .is_some_and(|k| !k.trim().is_empty())
                || std::env::var(&p.env).is_ok_and(|k| !k.trim().is_empty()),
            reusable_from: p
                .key_from
                .iter()
                .filter(|h| harness_key(h, &p.id).is_some())
                .cloned()
                .collect(),
        })
        .collect()
}

#[cfg(test)]
pub(crate) mod tests;
