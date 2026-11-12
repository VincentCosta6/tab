//! Reading cost and Claude plan limits out of what agents report.

use chrono::{TimeZone, Utc};
use serde_json::Value;
use tab_domain::runner::JobUsage;
use tab_domain::workflow::Billing;

use crate::store::{NewUsage, PlanWindow, Store, TurnUsage};

/// Claude Code's `apiKeySource` is "none" when it runs on a Claude plan login.
pub fn billing_from_init(v: &Value) -> &'static str {
    match v["apiKeySource"].as_str() {
        Some("none") => "subscription",
        Some(_) => "api",
        None => "unknown",
    }
}

/// Stores the plan windows in a Claude Code `rate_limit_event`.
pub fn record_rate_limit(store: &Store, info: &Value) {
    let now = Utc::now();
    let window = |name: &str, w: &Value| PlanWindow {
        window: name.to_string(),
        utilization: w["utilization"].as_f64(),
        resets_at: w["resetsAt"]
            .as_i64()
            .and_then(|s| Utc.timestamp_opt(s, 0).single()),
        status: w["status"].as_str().unwrap_or("allowed").to_string(),
        updated_at: now,
    };
    let mut windows: Vec<PlanWindow> = info["unifiedWindows"]
        .as_object()
        .map(|m| m.iter().map(|(k, w)| window(k, w)).collect())
        .unwrap_or_default();
    if windows.is_empty()
        && let Some(name) = info["rateLimitType"].as_str()
    {
        windows.push(window(name, info));
    }
    for w in windows {
        if let Err(e) = store.plan_window_set(&w) {
            tracing::warn!(error = %e, "couldn't store plan window");
        }
    }
}

/// Stores the cost in a Claude Code `result` event.
pub fn record_claude_result(
    store: &Store,
    v: &Value,
    purpose: &'static str,
    session_id: Option<&str>,
    billing: &'static str,
) {
    let usage = &v["usage"];
    let tokens = |k: &str| usage[k].as_i64().unwrap_or(0);
    let model = v["modelUsage"].as_object().and_then(|m| {
        m.iter()
            .max_by(|a, b| {
                let c = |x: &Value| x["costUSD"].as_f64().unwrap_or(0.0);
                c(a.1).total_cmp(&c(b.1))
            })
            .map(|(k, _)| k.clone())
    });
    let u = NewUsage {
        purpose,
        session_id: session_id.map(str::to_string),
        agent: "claude_code".into(),
        model,
        billing,
        cost_usd: v["total_cost_usd"].as_f64().unwrap_or(0.0),
        input_tokens: tokens("input_tokens")
            + tokens("cache_creation_input_tokens")
            + tokens("cache_read_input_tokens"),
        output_tokens: tokens("output_tokens"),
    };
    if let Err(e) = store.usage_add(&u, Utc::now()) {
        tracing::warn!(error = %e, "couldn't store usage");
    }
}

/// Highest utilization across the plan's windows, refreshed in the last 6 hours.
pub fn plan_peak(store: &Store) -> Option<f64> {
    let fresh = Utc::now() - chrono::Duration::hours(6);
    store
        .plan_windows()
        .ok()?
        .into_iter()
        .filter(|w| w.updated_at > fresh && w.resets_at.is_none_or(|r| r > Utc::now()))
        .filter_map(|w| w.utilization)
        .reduce(f64::max)
}

/// A chat's turns added up for the job it ran. A chat on an API key costs
/// dollars; one on a Claude plan costs tokens only (Claude Code's dollar
/// figure there is what the API would have charged, not a bill). Turns
/// paid for different ways leave the billing unknown. The harness and
/// model are the latest turn's, so a chat that moved models reports the
/// one that finished.
pub fn job_usage(rows: &[TurnUsage]) -> JobUsage {
    let billing = |b: &str| match b {
        "api" => Billing::Api,
        "subscription" => Billing::Subscription,
        _ => Billing::Unknown,
    };
    let mut u = JobUsage {
        billing: rows
            .first()
            .map(|r| billing(&r.billing))
            .filter(|b| rows.iter().all(|r| billing(&r.billing) == *b))
            .unwrap_or_default(),
        ..Default::default()
    };
    for r in rows {
        u.input_tokens += r.input_tokens.max(0) as u64;
        u.output_tokens += r.output_tokens.max(0) as u64;
        if billing(&r.billing) != Billing::Subscription {
            u.cost_usd += r.cost_usd;
        }
        u.harness = r.agent.clone();
        if let Some(m) = r.model.as_ref().filter(|m| !m.is_empty()) {
            u.model = m.clone();
        }
    }
    u
}

/// How much of the Claude plan a chat used, 0.0 to 1.0: the most any one
/// plan window's utilization rose between `before` and `after`. Zero when
/// a window wasn't reported again after `since`, or reset in between.
/// The windows are the whole account's, so this only means something
/// when nothing else used the plan meanwhile; the caller checks that.
pub fn plan_share(
    before: &[PlanWindow],
    after: &[PlanWindow],
    since: chrono::DateTime<Utc>,
) -> f64 {
    after
        .iter()
        .filter(|a| a.updated_at >= since)
        .filter_map(|a| {
            let b = before.iter().find(|b| b.window == a.window)?;
            if b.resets_at != a.resets_at {
                return None;
            }
            Some(a.utilization? - b.utilization?)
        })
        .fold(0.0_f64, f64::max)
        .clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn row(billing: &str, cost: f64, model: &str) -> TurnUsage {
        TurnUsage {
            at: Utc::now(),
            agent: "claude_code".into(),
            model: Some(model.into()),
            billing: billing.into(),
            cost_usd: cost,
            input_tokens: 1000,
            output_tokens: 100,
        }
    }

    #[test]
    fn a_chat_on_an_api_key_costs_dollars() {
        let u = job_usage(&[
            row("api", 0.5, "claude-sonnet-5"),
            row("api", 0.25, "claude-opus-5-5"),
        ]);
        assert_eq!(u.billing, Billing::Api);
        assert!((u.cost_usd - 0.75).abs() < 1e-9);
        assert_eq!((u.input_tokens, u.output_tokens), (2000, 200));
        assert_eq!(
            (u.harness.as_str(), u.model.as_str()),
            ("claude_code", "claude-opus-5-5")
        );
    }

    #[test]
    fn a_chat_on_a_plan_costs_tokens_not_dollars() {
        let u = job_usage(&[
            row("subscription", 0.5, "m"),
            row("subscription", 0.25, "m"),
        ]);
        assert_eq!(u.billing, Billing::Subscription);
        assert_eq!(u.cost_usd, 0.0);
        assert_eq!(u.input_tokens, 2000);
    }

    #[test]
    fn mixed_or_unreported_billing_is_unknown_and_keeps_what_was_billed() {
        let u = job_usage(&[row("subscription", 0.5, "m"), row("api", 0.25, "m")]);
        assert_eq!(u.billing, Billing::Unknown);
        assert!((u.cost_usd - 0.25).abs() < 1e-9);
        let u = job_usage(&[row("unknown", 0.1, "")]);
        assert_eq!(u.billing, Billing::Unknown);
        assert_eq!(u.model, "");
        assert_eq!(job_usage(&[]), JobUsage::default());
    }

    #[test]
    fn plan_share_is_the_biggest_rise_in_a_window_reported_since_the_start() {
        let start = Utc::now() - Duration::minutes(30);
        let resets = Some(start + Duration::hours(4));
        let w = |name: &str, u: f64, at, resets_at| PlanWindow {
            window: name.into(),
            utilization: Some(u),
            resets_at,
            status: "allowed".into(),
            updated_at: at,
        };
        let before = [
            w("five_hour", 0.20, start - Duration::hours(1), resets),
            w("seven_day", 0.50, start - Duration::hours(1), None),
        ];
        let after = [
            w("five_hour", 0.35, start + Duration::minutes(20), resets),
            w("seven_day", 0.52, start + Duration::minutes(20), None),
        ];
        assert!((plan_share(&before, &after, start) - 0.15).abs() < 1e-9);
        // Not reported again since the job started: nothing to measure.
        assert_eq!(plan_share(&before, &before, start), 0.0);
        // A window that reset in between says nothing.
        let reset = [w("five_hour", 0.05, start + Duration::minutes(20), None)];
        assert_eq!(plan_share(&before, &reset, start), 0.0);
    }
}
