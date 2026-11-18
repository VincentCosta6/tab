//! The plan an agent proposes and the person approves (phase 2 of
//! `docs/agent-browser.md`): which sites it may change, which it only reads,
//! and the steps that change things, with their exact values. The gate
//! ([`super::gate`]) lets through only what matches an approved plan.

use serde::{Deserialize, Serialize};

/// Steps per plan, and characters per value, at most.
pub const MAX_STEPS: usize = 40;
pub const MAX_VALUE: usize = 2_000;
pub const MAX_SITES: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Do {
    /// Go somewhere. With `url`, tab can do it; without, it describes where.
    Navigate,
    Click,
    Type,
    /// A click that sends a form (Save, Create, Submit).
    Submit,
    /// Pick an option in a select box (`value` is the option's label).
    Select,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Step {
    #[serde(rename = "do")]
    pub action: Do,
    /// The button or link, by its label (`click`, `submit`).
    #[serde(default)]
    pub what: Option<String>,
    /// The field, by its label (`type`, `select`).
    #[serde(default)]
    pub field: Option<String>,
    /// What's typed or picked, exactly.
    #[serde(default)]
    pub value: Option<String>,
    /// Where to go (`navigate`), if known.
    #[serde(default)]
    pub url: Option<String>,
    /// Where, in words (`navigate` without a URL).
    #[serde(default)]
    pub r#where: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Plan {
    pub goal: String,
    /// Sites (registrable domains) where it may click, type and submit, and
    /// which may be lent a login.
    #[serde(default)]
    pub write: Vec<String>,
    /// Sites it only reads (any public site reads anyway; listing them is
    /// for the person).
    #[serde(default)]
    pub read: Vec<String>,
    pub steps: Vec<Step>,
    /// Write sites the person is asked to lend a login for.
    #[serde(default)]
    pub needs_login: Vec<String>,
}

/// A host or URL as its registrable domain.
pub fn site_of(s: &str) -> Option<String> {
    let s = s.trim().trim_end_matches('/');
    if s.is_empty() {
        return None;
    }
    let u = super::parse(s).ok()?;
    super::site(&u)
}

impl Plan {
    /// Checks and normalizes a plan the agent sent: sites as registrable
    /// domains, write and read apart, every write step complete.
    pub fn validate(mut self) -> anyhow::Result<Self> {
        anyhow::ensure!(!self.goal.trim().is_empty(), "say the plan's goal");
        anyhow::ensure!(!self.steps.is_empty(), "a plan needs at least one step");
        anyhow::ensure!(
            self.steps.len() <= MAX_STEPS,
            "keep a plan to {MAX_STEPS} steps; split the task"
        );
        let norm = |list: &[String]| -> anyhow::Result<Vec<String>> {
            let mut out: Vec<String> = Vec::new();
            for s in list {
                super::check_static(&super::parse(s)?)?;
                let site = site_of(s).ok_or_else(|| anyhow::anyhow!("{s} isn't a site"))?;
                if !out.contains(&site) {
                    out.push(site);
                }
            }
            Ok(out)
        };
        self.write = norm(&self.write)?;
        self.read = norm(&self.read)?;
        self.needs_login = norm(&self.needs_login)?;
        anyhow::ensure!(
            self.write.len() + self.read.len() <= MAX_SITES,
            "a plan can name at most {MAX_SITES} sites"
        );
        self.read.retain(|s| !self.write.contains(s));
        anyhow::ensure!(
            self.needs_login.iter().all(|s| self.write.contains(s)),
            "only write sites can be lent a login"
        );
        for (i, s) in self.steps.iter().enumerate() {
            let n = i + 1;
            match s.action {
                Do::Click | Do::Submit => anyhow::ensure!(
                    s.what.as_deref().is_some_and(|w| !w.trim().is_empty()),
                    "step {n}: name what to click"
                ),
                Do::Type | Do::Select => {
                    anyhow::ensure!(
                        s.field.as_deref().is_some_and(|w| !w.trim().is_empty()),
                        "step {n}: name the field"
                    );
                    anyhow::ensure!(
                        s.value
                            .as_deref()
                            .is_some_and(|v| v.chars().count() <= MAX_VALUE),
                        "step {n}: give the exact value (at most {MAX_VALUE} characters)"
                    );
                }
                Do::Navigate => {
                    if let Some(u) = &s.url {
                        let u = super::parse(u)?;
                        super::check_static(&u)?;
                    } else {
                        anyhow::ensure!(
                            s.r#where.as_deref().is_some_and(|w| !w.trim().is_empty()),
                            "step {n}: give a url or say where"
                        );
                    }
                }
            }
        }
        anyhow::ensure!(
            self.write.is_empty() == self.steps.iter().all(|s| s.action == Do::Navigate),
            "steps that click, type or submit need a write site, and write sites need such steps"
        );
        Ok(self)
    }

    pub fn is_write(&self, site: &str) -> bool {
        self.write.iter().any(|s| s == site)
    }

    /// Every exact value the plan types or picks.
    pub fn values(&self) -> impl Iterator<Item = &str> {
        self.steps.iter().filter_map(|s| s.value.as_deref())
    }

    /// Whether `next` only takes away from `self`: no new sites, steps or
    /// values. A narrowing needs no approval.
    pub fn narrows(&self, next: &Plan) -> bool {
        next.write.iter().all(|s| self.write.contains(s))
            && next
                .needs_login
                .iter()
                .all(|s| self.needs_login.contains(s))
            && next.steps.iter().all(|s| self.steps.contains(s))
    }

    /// What changed, for the card on a revision: added and removed sites and steps.
    pub fn diff(&self, next: &Plan) -> serde_json::Value {
        let added_sites: Vec<&String> = next
            .write
            .iter()
            .filter(|s| !self.write.contains(s))
            .collect();
        let removed_sites: Vec<&String> = self
            .write
            .iter()
            .filter(|s| !next.write.contains(s))
            .collect();
        let added: Vec<&Step> = next
            .steps
            .iter()
            .filter(|s| !self.steps.contains(s))
            .collect();
        let removed: Vec<&Step> = self
            .steps
            .iter()
            .filter(|s| !next.steps.contains(s))
            .collect();
        serde_json::json!({
            "added_write_sites": added_sites,
            "removed_write_sites": removed_sites,
            "added_steps": added,
            "removed_steps": removed,
        })
    }
}

/// A label as compared: lower case, letters and digits, single spaces.
pub fn norm(s: &str) -> String {
    let mut out = String::new();
    let mut space = false;
    for c in s.chars() {
        if c.is_alphanumeric() {
            if space && !out.is_empty() {
                out.push(' ');
            }
            space = false;
            out.extend(c.to_lowercase());
        } else {
            space = true;
        }
    }
    out
}

/// How an element's labels match a step's label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Match {
    No,
    /// One contains the other ("Add record" / "+ Add Record now").
    Near,
    Exact,
}

pub fn label_match(step_label: &str, element_labels: &[&str]) -> Match {
    let want = norm(step_label);
    if want.is_empty() {
        return Match::No;
    }
    let mut best = Match::No;
    for l in element_labels {
        let l = norm(l);
        if l.is_empty() {
            continue;
        }
        if l == want {
            return Match::Exact;
        }
        // Whole words only, and the shorter must be most of the longer:
        // "Save" doesn't near-match "Save and delete everything".
        let (short, long) = if l.len() < want.len() {
            (&l, &want)
        } else {
            (&want, &l)
        };
        let words = |s: &str| format!(" {s} ");
        if words(long).contains(&words(short)) && short.len() * 2 >= long.len() {
            best = Match::Near;
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(json: serde_json::Value) -> anyhow::Result<Plan> {
        serde_json::from_value::<Plan>(json)?.validate()
    }

    fn dns() -> Plan {
        plan(serde_json::json!({
            "goal": "Point www at the app",
            "write": ["https://dash.cloudflare.com/abc"],
            "read": ["developers.cloudflare.com", "cloudflare.com"],
            "steps": [
                { "do": "navigate", "where": "DNS records" },
                { "do": "click", "what": "Add record" },
                { "do": "type", "field": "Name", "value": "www" },
                { "do": "submit", "what": "Save" }
            ],
            "needs_login": ["dash.cloudflare.com"]
        }))
        .unwrap()
    }

    #[test]
    fn sites_become_registrable_domains_and_read_excludes_write() {
        let p = dns();
        assert_eq!(p.write, ["cloudflare.com"]);
        assert!(p.read.is_empty(), "{:?}", p.read);
        assert_eq!(p.needs_login, ["cloudflare.com"]);
        assert!(p.is_write("cloudflare.com"));
    }

    #[test]
    fn incomplete_or_unsafe_plans_are_refused() {
        let base = |steps: serde_json::Value, write: &[&str]| {
            plan(serde_json::json!({ "goal": "x", "write": write, "steps": steps }))
        };
        assert!(
            base(
                serde_json::json!([{ "do": "type", "field": "Name" }]),
                &["a.com"]
            )
            .is_err()
        );
        assert!(base(serde_json::json!([{ "do": "click" }]), &["a.com"]).is_err());
        assert!(base(serde_json::json!([{ "do": "click", "what": "Go" }]), &[]).is_err());
        assert!(
            base(
                serde_json::json!([{ "do": "click", "what": "Go" }]),
                &["localhost"]
            )
            .is_err()
        );
        assert!(
            base(
                serde_json::json!([{ "do": "click", "what": "Go" }]),
                &["10.0.0.1"]
            )
            .is_err()
        );
        assert!(
            base(
                serde_json::json!([{ "do": "navigate", "url": "file:///etc" }]),
                &[]
            )
            .is_err()
        );
        assert!(base(serde_json::json!([]), &["a.com"]).is_err());
        assert!(
            plan(
                serde_json::json!({ "goal": "x", "write": ["a.com"], "needs_login": ["b.com"],
                "steps": [{ "do": "click", "what": "Go" }] })
            )
            .is_err()
        );
        assert!(
            base(
                serde_json::json!([{ "do": "navigate", "where": "docs" }]),
                &[]
            )
            .is_ok()
        );
    }

    #[test]
    fn narrowing_needs_no_approval_but_anything_new_does() {
        let p = dns();
        let mut fewer = p.clone();
        fewer.steps.remove(2);
        assert!(p.narrows(&fewer));
        let mut more = p.clone();
        more.steps.push(Step {
            action: Do::Type,
            what: None,
            field: Some("Target".into()),
            value: Some("evil.example".into()),
            url: None,
            r#where: None,
        });
        assert!(!p.narrows(&more));
        let mut other_value = p.clone();
        other_value.steps[2].value = Some("api".into());
        assert!(!p.narrows(&other_value));
        let mut new_site = p.clone();
        new_site.write.push("github.com".into());
        assert!(!p.narrows(&new_site));
        assert_eq!(
            p.diff(&new_site)["added_write_sites"],
            serde_json::json!(["github.com"])
        );
    }

    #[test]
    fn labels_match_exactly_or_near_on_whole_words() {
        assert_eq!(label_match("Add record", &["Add record"]), Match::Exact);
        assert_eq!(label_match("Add record", &["+ Add Record"]), Match::Exact);
        assert_eq!(label_match("Add record", &["Add record now"]), Match::Near);
        assert_eq!(
            label_match("Save", &["Save and delete everything"]),
            Match::No
        );
        assert_eq!(label_match("Save", &["Saved"]), Match::No);
        assert_eq!(label_match("Target", &["", "Target host"]), Match::Near);
        assert_eq!(label_match("Name", &["Record name"]), Match::No);
        assert_eq!(label_match("", &["x"]), Match::No);
    }
}
