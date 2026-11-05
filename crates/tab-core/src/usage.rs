//! Reading cost and Claude plan limits out of what agents report.

use chrono::{TimeZone, Utc};
use serde_json::Value;

use crate::store::{NewUsage, PlanWindow, Store};

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
