//! Monthly spend caps on a workflow ([`Limits`]): a cap in API dollars and
//! a cap in subscription tokens. API-key sessions are measured in dollars
//! and subscription sessions in usage, and the two are never converted.
//!
//! Before starting a run the runtime adds up the workflow's spend in the
//! current UTC calendar month ([`rollup::month_spend`]: every run that
//! isn't a dry run, including ones still going) and refuses the run when
//! either is at or over its cap ([`reached`]), recording it as held back
//! the way the hourly limit is. A run already going is never stopped, so a
//! month can end a little over; dry runs aren't capped. 0 means no cap.

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

use super::stats::Spend;
use super::{Issue, Limits, rollup};

/// What a cap meter shows: this month's spend against the caps.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct CapUsage {
    /// The first day of the UTC month.
    pub month: Option<NaiveDate>,
    pub spend: Spend,
    pub cap_usd: f64,
    pub cap_plan_tokens: u64,
}

impl CapUsage {
    /// `None` when the workflow has no cap.
    pub fn of(limits: &Limits, spend: Spend, now: DateTime<Utc>) -> Option<Self> {
        has_cap(limits).then(|| Self {
            month: Some(rollup::month_start(now)),
            spend,
            cap_usd: limits.monthly_cap_usd,
            cap_plan_tokens: limits.monthly_cap_plan_tokens,
        })
    }
}

pub fn has_cap(limits: &Limits) -> bool {
    limits.monthly_cap_usd > 0.0 || limits.monthly_cap_plan_tokens > 0
}

/// Why a run can't start this month, or `None` while it's under its caps.
pub fn reached(limits: &Limits, spent: &Spend) -> Option<String> {
    if limits.monthly_cap_usd > 0.0 && spent.cost_usd >= limits.monthly_cap_usd {
        return Some(format!(
            "This month's ${:.2} spend cap is reached; runs start again on the 1st (UTC).",
            limits.monthly_cap_usd
        ));
    }
    let tokens = spent.plan_input_tokens + spent.plan_output_tokens;
    if limits.monthly_cap_plan_tokens > 0 && tokens >= limits.monthly_cap_plan_tokens {
        return Some(format!(
            "This month's {} subscription token cap is reached; runs start again on the 1st (UTC).",
            grouped(limits.monthly_cap_plan_tokens)
        ));
    }
    None
}

/// Problems with the caps, for [`validate`](super::validate).
pub fn check(limits: &Limits) -> Vec<Issue> {
    let usd = limits.monthly_cap_usd;
    if !usd.is_finite() || usd < 0.0 {
        return vec![Issue::err(
            None,
            "The monthly spend cap is a dollar amount, 0 or more (0 means no cap).",
        )];
    }
    Vec::new()
}

/// `1234567` as `1,234,567`.
fn grouped(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;
    use crate::workflow::{Trigger, Workflow, validate};

    fn limits(usd: f64, tokens: u64) -> Limits {
        Limits {
            monthly_cap_usd: usd,
            monthly_cap_plan_tokens: tokens,
            ..Default::default()
        }
    }

    fn spent(usd: f64, plan_in: u64, plan_out: u64) -> Spend {
        Spend {
            cost_usd: usd,
            input_tokens: plan_in + plan_out + 5_000,
            output_tokens: 0,
            plan_input_tokens: plan_in,
            plan_output_tokens: plan_out,
            plan_share: 0.5,
        }
    }

    #[test]
    fn a_cap_stops_runs_at_or_over_it_and_zero_means_none() {
        assert_eq!(reached(&limits(0.0, 0), &spent(1e6, 1 << 40, 0)), None);
        assert_eq!(reached(&limits(20.0, 0), &spent(19.99, 0, 0)), None);
        assert_eq!(
            reached(&limits(20.0, 0), &spent(20.0, 0, 0)).unwrap(),
            "This month's $20.00 spend cap is reached; runs start again on the 1st (UTC)."
        );
        assert!(reached(&limits(20.0, 0), &spent(31.5, 0, 0)).is_some());
        // Subscription usage is counted in tokens and never as dollars, and
        // API tokens don't count against the token cap.
        assert_eq!(
            reached(&limits(0.0, 2_000_000), &spent(500.0, 1_000_000, 999_999)),
            None
        );
        assert_eq!(
            reached(&limits(0.0, 2_000_000), &spent(0.0, 1_500_000, 500_000)).unwrap(),
            "This month's 2,000,000 subscription token cap is reached; runs start again on the 1st (UTC)."
        );
    }

    #[test]
    fn usage_shows_only_with_a_cap() {
        let now = Utc.with_ymd_and_hms(2026, 11, 12, 9, 0, 0).unwrap();
        assert_eq!(CapUsage::of(&limits(0.0, 0), spent(3.0, 0, 0), now), None);
        let u = CapUsage::of(&limits(20.0, 0), spent(3.0, 0, 0), now).unwrap();
        assert_eq!(u.month, NaiveDate::from_ymd_opt(2026, 11, 1));
        assert_eq!((u.cap_usd, u.cap_plan_tokens), (20.0, 0));
    }

    #[test]
    fn validation_rejects_negative_caps() {
        let mut w = Workflow {
            name: "W".into(),
            trigger: Trigger::Manual,
            ..Default::default()
        };
        let errors = |w: &Workflow| {
            validate(w, &[], &[], &[])
                .into_iter()
                .filter(|i| i.error)
                .map(|i| i.message)
                .collect::<Vec<_>>()
        };
        assert!(errors(&w).is_empty());
        w.limits.monthly_cap_usd = -1.0;
        assert_eq!(errors(&w).len(), 1);
        w.limits.monthly_cap_usd = f64::NAN;
        assert_eq!(errors(&w).len(), 1);
        w.limits.monthly_cap_usd = 25.0;
        w.limits.monthly_cap_plan_tokens = 3_000_000;
        assert!(errors(&w).is_empty());
        // A negative token cap doesn't parse.
        assert!(
            serde_json::from_str::<Limits>(
                r#"{"max_runs_per_hour":5,"monthly_cap_plan_tokens":-1}"#
            )
            .is_err()
        );
    }

    #[test]
    fn limits_from_before_caps_still_parse() {
        let old: Limits =
            serde_json::from_str(r#"{"max_runs_per_hour":12,"cooldown_minutes":5}"#).unwrap();
        assert_eq!(
            old,
            Limits {
                max_runs_per_hour: 12,
                cooldown_minutes: 5,
                ..Default::default()
            }
        );
        assert!(!has_cap(&old));
    }

    #[test]
    fn big_numbers_are_grouped() {
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(1_000), "1,000");
        assert_eq!(grouped(12_345_678), "12,345,678");
    }
}
