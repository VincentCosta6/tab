//! Model families: a named, ordered list of harness and model pairs an
//! agent step can ask for instead of pinning one.
//!
//! tab ships three ([`BUILT_IN`]: `fast`, `balanced`, `deep`), seeded from
//! the harness catalog; the person can edit and reorder them and add their
//! own. A saved family with a built-in's id replaces it. Families sync
//! through the hub like other settings, so every runner sees the same list.
//!
//! The workflow copies the family's candidates onto the job when it hands
//! the step out; the runner that claims it runs the first candidate whose
//! harness it has ([`crate::runner::JobSpec::plan`]) and moves on to the
//! next only when one fails for a model reason ([`model_trouble`]).

use serde::{Deserialize, Serialize};

/// The families tab ships, in the order they're shown.
pub const BUILT_IN: [&str; 3] = ["fast", "balanced", "deep"];

/// One harness and model a family may run on.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Candidate {
    /// A harness id (`claude_code`, `opencode`, `codex`, `pi`).
    pub harness: String,
    /// The model to ask for; empty means the harness's default.
    #[serde(default)]
    pub model: String,
}

impl Candidate {
    pub fn new(harness: &str, model: &str) -> Self {
        Self {
            harness: harness.into(),
            model: model.into(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelFamily {
    /// What steps name it by: letters, digits, `-` and `_`.
    pub id: String,
    /// Shown to the person; empty shows the id.
    #[serde(default)]
    pub name: String,
    /// Tried in this order.
    #[serde(default)]
    pub candidates: Vec<Candidate>,
}

impl ModelFamily {
    pub fn is_built_in(&self) -> bool {
        BUILT_IN.contains(&self.id.as_str())
    }
}

/// The families to offer: the built-ins in their order (each replaced by a
/// saved copy with its id), then the person's own by name.
pub fn merge(built_in: Vec<ModelFamily>, saved: Vec<ModelFamily>) -> Vec<ModelFamily> {
    let mut out: Vec<ModelFamily> = built_in
        .into_iter()
        .map(|b| saved.iter().find(|s| s.id == b.id).cloned().unwrap_or(b))
        .collect();
    let mut own: Vec<ModelFamily> = saved
        .into_iter()
        .filter(|s| !out.iter().any(|o| o.id == s.id))
        .collect();
    own.sort_by_key(|a| a.name.to_lowercase());
    out.extend(own);
    out
}

/// What's wrong with a family before saving, if anything.
pub fn check(f: &ModelFamily) -> Option<String> {
    if f.id.is_empty()
        || !f
            .id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Some(format!(
            "Family ids are letters, digits, - and _, got {:?}.",
            f.id
        ));
    }
    if f.candidates.is_empty() {
        return Some("Add at least one model to the family.".into());
    }
    if f.candidates.iter().any(|c| c.harness.trim().is_empty()) {
        return Some("Pick a harness for every model in the family.".into());
    }
    None
}

/// Why a candidate failed in a way the next one might not, from the error
/// its harness reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelTrouble {
    /// The model doesn't exist, or this account can't use it.
    Unavailable,
    /// Too many requests, or the provider is overloaded.
    RateLimited,
    /// The plan's usage window or credits are used up.
    PlanUsedUp,
}

/// Classifies a harness's error text. Only these reasons move a job on to
/// the family's next model; anything else (a failing test, a crash, a
/// missing sign-in) is the job's own failure. It reads text, so it knows
/// the wordings tab's adapters pass on from Claude Code, Codex, OpenCode
/// and Pi, and the providers' usual ones; an unfamiliar wording counts as
/// an ordinary failure.
pub fn model_trouble(error: &str) -> Option<ModelTrouble> {
    let e = error.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| e.contains(w));
    let codes: Vec<&str> = e.split(|c: char| !c.is_ascii_alphanumeric()).collect();
    if has(&[
        "usage limit",
        "limit reached",
        "limit will reset",
        "insufficient_quota",
        "exceeded your current quota",
        "quota exceeded",
        "credit balance is too low",
        "out of credits",
        "plan limit",
    ]) {
        return Some(ModelTrouble::PlanUsedUp);
    }
    if has(&[
        "rate limit",
        "rate_limit",
        "ratelimit",
        "too many requests",
        "overloaded",
        "resource_exhausted",
    ]) || codes.iter().any(|c| *c == "429" || *c == "529")
    {
        return Some(ModelTrouble::RateLimited);
    }
    let about_model = e.contains("model");
    if about_model
        && has(&[
            "not found",
            "not_found",
            "does not exist",
            "not available",
            "unavailable",
            "not supported",
            "unsupported",
            "unknown model",
            "invalid model",
            "no access",
            "not have access",
            "not allowed",
        ])
    {
        return Some(ModelTrouble::Unavailable);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fam(id: &str, name: &str, cands: &[(&str, &str)]) -> ModelFamily {
        ModelFamily {
            id: id.into(),
            name: name.into(),
            candidates: cands.iter().map(|(h, m)| Candidate::new(h, m)).collect(),
        }
    }

    #[test]
    fn saved_families_replace_built_ins_and_the_persons_own_follow_by_name() {
        let built = vec![
            fam("fast", "Fast", &[("claude_code", "haiku")]),
            fam("deep", "Deep", &[("claude_code", "opus")]),
        ];
        let saved = vec![
            fam("zed", "Zed", &[("codex", "")]),
            fam("deep", "Deep", &[("codex", "gpt"), ("claude_code", "opus")]),
            fam("cheap", "Cheap", &[("pi", "")]),
        ];
        let all = merge(built, saved);
        let ids: Vec<_> = all.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(ids, ["fast", "deep", "cheap", "zed"]);
        assert_eq!(all[1].candidates[0], Candidate::new("codex", "gpt"));
        assert!(all[0].is_built_in() && !all[2].is_built_in());
    }

    #[test]
    fn a_family_needs_an_id_and_a_harness_for_every_model() {
        assert_eq!(check(&fam("mine", "", &[("codex", "")])), None);
        assert!(check(&fam("my family", "", &[("codex", "")])).is_some());
        assert!(check(&fam("mine", "", &[])).is_some());
        assert!(check(&fam("mine", "", &[(" ", "x")])).is_some());
    }

    #[test]
    fn model_reasons_are_told_apart_from_ordinary_failures() {
        use ModelTrouble::*;
        let cases = [
            ("You've hit your usage limit.", Some(PlanUsedUp)),
            ("Claude AI usage limit reached|1760000000", Some(PlanUsedUp)),
            (
                "insufficient_quota: You exceeded your current quota",
                Some(PlanUsedUp),
            ),
            ("Your credit balance is too low", Some(PlanUsedUp)),
            ("API Error: 429 Too Many Requests", Some(RateLimited)),
            ("rate_limit_error: slow down", Some(RateLimited)),
            (
                "API Error: 529 {\"type\":\"overloaded_error\"}",
                Some(RateLimited),
            ),
            (
                "The model gpt-9 does not exist or you do not have access to it.",
                Some(Unavailable),
            ),
            ("model_not_found", Some(Unavailable)),
            ("Model not found: anthropic/claude-x", Some(Unavailable)),
            ("Unknown model: claude-x", Some(Unavailable)),
            ("cargo test failed: 3 tests failed", None),
            ("No API key for anthropic", None),
            ("The agent stopped with an error.", None),
            ("Process exited with code 1429", None),
        ];
        for (text, want) in cases {
            assert_eq!(model_trouble(text), want, "{text}");
        }
    }
}
