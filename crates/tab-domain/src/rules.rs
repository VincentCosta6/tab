use serde::{Deserialize, Serialize};

use crate::model::{IncomingEvent, UserProfile};

/// Deterministic scoring rules. Serializable so the UI (and later MCP) can edit them.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RankingRules {
    /// Base score per kind. Keys are exact kinds (`github.review_requested`)
    /// or prefixes ending in `.*` (`slack.*`). Most specific match wins.
    pub kinds: Vec<KindScore>,
    pub default_score: u8,
    pub keywords: Vec<KeywordRule>,
    /// Actors whose items get a boost (your manager, your on-call partner).
    #[serde(default)]
    pub vip_actors: Vec<String>,
    #[serde(default = "default_vip_boost")]
    pub vip_boost: i16,
    /// Contexts (channels, repos, senders) that are muted: score 0, no AI call.
    #[serde(default)]
    pub muted: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KindScore {
    pub kind: String,
    pub score: u8,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KeywordRule {
    /// Case-insensitive substring matched against title and body.
    pub contains: String,
    pub delta: i16,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RuleResult {
    pub score: u8,
    pub reason: String,
    pub muted: bool,
    /// Which rules fired, for the AI layer and for debugging.
    pub fired: Vec<String>,
}

fn default_vip_boost() -> i16 {
    15
}

fn k(kind: &str, score: u8, label: &str) -> KindScore {
    KindScore {
        kind: kind.into(),
        score,
        label: label.into(),
    }
}

fn kw(contains: &str, delta: i16) -> KeywordRule {
    KeywordRule {
        contains: contains.into(),
        delta,
    }
}

impl Default for RankingRules {
    fn default() -> Self {
        Self {
            kinds: vec![
                k("agent.needs_input", 75, "An agent is blocked on you"),
                k("tab.scheduled", 72, "Something you scheduled is due"),
                k("tab.task", 62, "A task you created"),
                k(
                    "github.review_requested",
                    70,
                    "Someone is waiting on your review",
                ),
                k(
                    "agent.needs_review",
                    65,
                    "An agent finished work for you to review",
                ),
                k("github.ci_failed", 65, "CI failed on your change"),
                k("github.ci_passed", 15, "CI passed"),
                k("github.security", 65, "Security alert"),
                k("github.mention", 60, "You were mentioned"),
                k("slack.mention", 60, "You were mentioned"),
                k("slack.dm", 60, "Direct message"),
                k("meeting.action_item", 55, "Action item assigned to you"),
                k("github.assign", 55, "Assigned to you"),
                k("github.comment", 45, "New activity on a thread you're in"),
                k("email.direct", 45, "Email addressed to you"),
                k("email.cc", 30, "Email you're copied on"),
                k("email.newsletter", 10, "Newsletter"),
                k("email.automated", 15, "Automated email"),
                k("agent.*", 40, "Agent session update"),
                k("github.*", 35, "GitHub activity"),
                k("slack.*", 30, "Slack activity"),
                k("email.*", 20, "Email"),
            ],
            default_score: 30,
            keywords: vec![
                kw("outage", 20),
                kw("incident", 15),
                kw("urgent", 15),
                kw("asap", 15),
                kw("blocked", 12),
                kw("blocking", 12),
                kw("prod", 8),
                kw("release", 5),
                kw("newsletter", -20),
                kw("digest", -15),
                kw("unsubscribe", -15),
                kw("reminder", -5),
            ],
            vip_actors: Vec::new(),
            vip_boost: default_vip_boost(),
            muted: Vec::new(),
        }
    }
}

impl RankingRules {
    fn kind_rule(&self, kind: &str) -> Option<&KindScore> {
        // Exact match first, then the longest matching prefix.
        self.kinds.iter().find(|k| k.kind == kind).or_else(|| {
            self.kinds
                .iter()
                .filter_map(|k| {
                    k.kind
                        .strip_suffix(".*")
                        .filter(|p| kind.starts_with(&format!("{p}.")))
                        .map(|p| (p.len(), k))
                })
                .max_by_key(|(len, _)| *len)
                .map(|(_, k)| k)
        })
    }

    /// Pure function of the event, the profile and the rules. No clock, no I/O.
    pub fn score(&self, e: &IncomingEvent, profile: &UserProfile) -> RuleResult {
        let lower = |s: &str| s.to_lowercase();
        let context = e.context.as_deref().map(lower).unwrap_or_default();
        let actor = e.actor.as_deref().map(lower).unwrap_or_default();
        if self
            .muted
            .iter()
            .map(|m| lower(m))
            .any(|m| !m.is_empty() && (m == context || m == actor))
        {
            return RuleResult {
                score: 0,
                reason: "Muted".into(),
                muted: true,
                fired: vec!["muted".into()],
            };
        }

        let mut fired = Vec::new();
        let (mut score, reason) = match self.kind_rule(&e.kind) {
            Some(k) => {
                fired.push(format!("kind {} = {}", k.kind, k.score));
                (k.score as i16, k.label.clone())
            }
            None => (self.default_score as i16, "New activity".to_string()),
        };

        let text = lower(&format!("{} {}", e.title, e.body.as_deref().unwrap_or("")));
        for rule in &self.keywords {
            if !rule.contains.is_empty() && text.contains(&lower(&rule.contains)) {
                score += rule.delta;
                fired.push(format!("keyword \"{}\" {:+}", rule.contains, rule.delta));
            }
        }
        if !actor.is_empty() && self.vip_actors.iter().any(|v| actor.contains(&lower(v))) {
            score += self.vip_boost;
            fired.push(format!("vip {:+}", self.vip_boost));
        }
        if profile
            .handles
            .iter()
            .any(|h| !h.is_empty() && text.contains(&format!("@{}", lower(h))))
        {
            score += 5;
            fired.push("mentions you +5".into());
        }

        // Priority set explicitly on a tab task (or by an agent) is the
        // person's own call, so it is a strong signal.
        let explicit = e.metadata.get("priority").and_then(|p| p.as_str());
        let delta = match explicit {
            Some("urgent") => 30,
            Some("high") => 15,
            Some("low") => -20,
            _ => 0,
        };
        if delta != 0 {
            score += delta;
            fired.push(format!(
                "priority {} {delta:+}",
                explicit.unwrap_or_default()
            ));
        }

        RuleResult {
            score: score.clamp(0, 100) as u8,
            reason,
            muted: false,
            fired,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(kind: &str, title: &str) -> IncomingEvent {
        IncomingEvent {
            kind: kind.into(),
            title: title.into(),
            ..Default::default()
        }
    }

    #[test]
    fn deterministic_scores() {
        let r = RankingRules::default();
        let p = UserProfile::default();
        let a = r.score(&ev("github.review_requested", "Fix prod outage"), &p);
        assert_eq!(a.score, 98);
        assert_eq!(
            a,
            r.score(&ev("github.review_requested", "Fix prod outage"), &p),
            "same input, same output"
        );
        assert_eq!(
            r.score(&ev("slack.channel", "hello"), &p).score,
            30,
            "prefix rule"
        );
        assert_eq!(
            r.score(&ev("email.newsletter", "This week's newsletter"), &p)
                .score,
            0
        );
        assert_eq!(r.score(&ev("unknown.thing", "x"), &p).score, 30);
    }

    #[test]
    fn mutes_and_vips() {
        let r = RankingRules {
            muted: vec!["#random".into()],
            vip_actors: vec!["dana".into()],
            ..Default::default()
        };
        let p = UserProfile::default();
        let mut e = ev("slack.mention", "hi");
        e.context = Some("#random".into());
        assert!(r.score(&e, &p).muted);
        e.context = None;
        e.actor = Some("Dana K".into());
        assert_eq!(r.score(&e, &p).score, 75);
    }
}
