//! Words and numbers the builder, Insights and Factory share: labels for
//! triggers, steps, runs and conditions; whole percentages, durations,
//! dollars and tokens. The counterpart of `insights/format.ts` and the
//! label tables at the top of `WorkflowsTab.tsx`.

use serde_json::Value;
use tab_core::model::{Change, ConnectorInfo};
use tab_core::workflow::stats::{Counts, Health, Spend};
use tab_core::workflow::{Op, RunStatus, StepStatus, Trigger};

use crate::theme::{Theme, Tone};

/// Fewer runs than this and a rate says nothing (`HEALTH_MIN_RUNS`).
pub const MIN_RUNS: u32 = 3;

pub const CHANGES: [(Change, &str); 7] = [
    (Change::Ingested, "New"),
    (Change::Updated, "Updated"),
    (Change::Ranked, "Ranked"),
    (Change::Snoozed, "Snoozed"),
    (Change::Woke, "Woke"),
    (Change::Done, "Done"),
    (Change::Reopened, "Reopened"),
];

/// Condition operators, their words, and whether they take a value.
pub const OPS: [(Op, &str, bool); 11] = [
    (Op::Equals, "is", true),
    (Op::NotEquals, "is not", true),
    (Op::Contains, "contains", true),
    (Op::NotContains, "doesn't contain", true),
    (Op::StartsWith, "starts with", true),
    (Op::Matches, "matches (*)", true),
    (Op::In, "is one of", true),
    (Op::Gt, ">", true),
    (Op::Lt, "<", true),
    (Op::Exists, "is set", false),
    (Op::Missing, "is empty", false),
];

pub fn op_label(op: Op) -> &'static str {
    OPS.iter().find(|o| o.0 == op).map_or("?", |o| o.1)
}

pub fn op_takes_value(op: Op) -> bool {
    OPS.iter().find(|o| o.0 == op).is_none_or(|o| o.2)
}

/// A JSON value as the person typed it: strings bare, the rest as JSON.
pub fn text(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        v => v.to_string(),
    }
}

pub fn connector_name(connectors: &[ConnectorInfo], id: &str) -> String {
    connectors
        .iter()
        .find(|c| c.descriptor.id == id)
        .map_or(id.to_string(), |c| c.descriptor.name.clone())
}

/// "When an item is new from GitHub", "Every 2 hours".
pub fn describe_trigger(t: &Trigger, connectors: &[ConnectorInfo]) -> String {
    match t {
        Trigger::Item {
            changes, sources, ..
        } => {
            let ch = changes
                .iter()
                .map(|c| {
                    CHANGES
                        .iter()
                        .find(|x| x.0 == *c)
                        .map_or("?", |x| x.1)
                        .to_lowercase()
                })
                .collect::<Vec<_>>()
                .join(" or ");
            let src = sources
                .iter()
                .map(|s| connector_name(connectors, s))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "When an item is {}{}",
                if ch.is_empty() { "…" } else { &ch },
                if src.is_empty() {
                    String::new()
                } else {
                    format!(" from {src}")
                }
            )
        }
        Trigger::Signal { name, .. } => format!(
            "When a workflow signals {}",
            if name.is_empty() { "…" } else { name }
        ),
        Trigger::Schedule { every_minutes: m } => {
            let m = *m;
            if m > 0 && m % 1440 == 0 {
                if m == 1440 {
                    "Every day".into()
                } else {
                    format!("Every {} days", m / 1440)
                }
            } else if m > 0 && m % 60 == 0 {
                if m == 60 {
                    "Every 1 hour".into()
                } else {
                    format!("Every {} hours", m / 60)
                }
            } else {
                format!("Every {m} minutes")
            }
        }
        Trigger::Manual => "When you run it".into(),
    }
}

pub fn run_tone(t: &Theme, s: RunStatus) -> Tone {
    match s {
        RunStatus::Succeeded => t.success,
        RunStatus::Failed => t.danger,
        RunStatus::Blocked | RunStatus::Throttled | RunStatus::LoopStopped => t.warning,
        RunStatus::Running => t.info,
        RunStatus::Waiting => t.accent,
        RunStatus::Stopped => t.neutral,
    }
}

pub fn run_label(s: RunStatus) -> &'static str {
    match s {
        RunStatus::Running => "Running",
        RunStatus::Waiting => "Waiting on an agent",
        RunStatus::Succeeded => "Ran",
        RunStatus::Stopped => "Stopped",
        RunStatus::Failed => "Failed",
        RunStatus::Blocked => "Needs your OK",
        RunStatus::Throttled => "Held back",
        RunStatus::LoopStopped => "Loop stopped",
    }
}

pub fn step_tone(t: &Theme, s: StepStatus) -> Tone {
    match s {
        StepStatus::Ok => t.success,
        StepStatus::Waiting | StepStatus::Dry => t.accent,
        StepStatus::Skipped | StepStatus::Stopped => t.neutral,
        StepStatus::Failed => t.danger,
        StepStatus::Blocked | StepStatus::Throttled => t.warning,
    }
}

pub fn step_label(s: StepStatus) -> &'static str {
    match s {
        StepStatus::Ok => "Done",
        StepStatus::Waiting => "Agent working",
        StepStatus::Skipped => "Skipped",
        StepStatus::Stopped => "Stopped here",
        StepStatus::Failed => "Failed",
        StepStatus::Blocked => "Blocked",
        StepStatus::Throttled => "Held back",
        StepStatus::Dry => "Would run",
    }
}

// ---------------------------------------------------------------------------
// Numbers
// ---------------------------------------------------------------------------

pub fn rate(n: u32, of: u32) -> Option<f64> {
    (of > 0).then(|| n as f64 / of as f64)
}

pub fn pct(r: Option<f64>) -> String {
    r.map_or("–".into(), |r| format!("{}%", (r * 100.).round() as i64))
}

pub fn success_rate(c: &Counts) -> Option<f64> {
    rate(c.succeeded, c.runs)
}
pub fn unattended_rate(c: &Counts) -> Option<f64> {
    rate(c.unattended, c.runs)
}
pub fn intervention_rate(c: &Counts) -> Option<f64> {
    rate(c.attended, c.runs)
}

pub fn duration(ms: u64) -> String {
    if ms < 1000 {
        return if ms < 50 {
            format!("{ms}ms")
        } else {
            format!("{:.1}s", ms as f64 / 1000.)
        };
    }
    let s = (ms as f64 / 1000.).round() as u64;
    if s < 60 {
        return format!("{s}s");
    }
    let (m, rs) = (s / 60, s % 60);
    if m < 10 {
        return if rs > 0 {
            format!("{m}m {rs}s")
        } else {
            format!("{m}m")
        };
    }
    if m < 60 {
        return format!("{m}m");
    }
    let (h, rm) = (m / 60, m % 60);
    if rm > 0 {
        format!("{h}h {rm}m")
    } else {
        format!("{h}h")
    }
}

pub fn usd(n: f64) -> String {
    if n > 0. && n < 0.01 {
        "<$0.01".into()
    } else {
        format!("${n:.2}")
    }
}

pub fn tokens(n: u64) -> String {
    if n < 1000 {
        format!("{n}")
    } else if n < 1_000_000 {
        if n < 10_000 {
            format!("{:.1}k", n as f64 / 1000.)
        } else {
            format!("{:.0}k", n as f64 / 1000.)
        }
    } else if n < 10_000_000 {
        format!("{:.1}M", n as f64 / 1e6)
    } else {
        format!("{:.0}M", n as f64 / 1e6)
    }
}

pub fn plan_share(share: f64) -> String {
    if share < 1. {
        format!("{}% of a window", ((share * 100.).round() as i64).max(1))
    } else {
        format!("{share:.1} windows")
    }
}

pub fn has_api(s: &Spend) -> bool {
    s.cost_usd > 0.
}
pub fn plan_tokens(s: &Spend) -> u64 {
    s.plan_input_tokens + s.plan_output_tokens
}
pub fn has_plan(s: &Spend) -> bool {
    plan_tokens(s) > 0
}

/// Dollars for API keys, plan tokens for subscriptions, both when both.
pub fn spend_short(s: &Spend, compact: bool) -> String {
    let mut parts = Vec::new();
    if has_api(s) {
        parts.push(usd(s.cost_usd));
    }
    if has_plan(s) {
        parts.push(format!(
            "{} plan {}",
            tokens(plan_tokens(s)),
            if compact { "tok" } else { "tokens" }
        ));
    }
    if parts.is_empty() {
        "Nothing".into()
    } else {
        parts.join(" + ")
    }
}

pub fn health_label(h: Health) -> &'static str {
    match h {
        Health::Healthy => "Healthy",
        Health::Degraded => "Degraded",
        Health::Failing => "Failing",
        Health::Unknown => "Too new to say",
    }
}

pub fn health_icon(h: Health) -> &'static str {
    match h {
        Health::Healthy => "check",
        Health::Degraded => "alert",
        Health::Failing => "octagon",
        Health::Unknown => "clock",
    }
}

pub fn health_tone(t: &Theme, h: Health) -> Tone {
    match h {
        Health::Healthy => t.success,
        Health::Degraded => t.warning,
        Health::Failing => t.danger,
        Health::Unknown => t.neutral,
    }
}

/// The same thresholds as `health`, for a rate on its own.
pub fn rate_tone(t: &Theme, r: Option<f64>, runs: u32) -> Tone {
    match r {
        Some(r) if runs >= MIN_RUNS => {
            if r >= 0.9 {
                t.success
            } else if r >= 0.5 {
                t.warning
            } else {
                t.danger
            }
        }
        _ => t.neutral,
    }
}

pub fn plural(n: u32, one: &str) -> String {
    format!("{n} {one}{}", if n == 1 { "" } else { "s" })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schedules_read_naturally() {
        let d = |m| describe_trigger(&Trigger::Schedule { every_minutes: m }, &[]);
        assert_eq!(d(1440), "Every day");
        assert_eq!(d(2880), "Every 2 days");
        assert_eq!(d(120), "Every 2 hours");
        assert_eq!(d(15), "Every 15 minutes");
    }

    #[test]
    fn numbers_read_like_the_tauri_app() {
        assert_eq!(duration(30), "30ms");
        assert_eq!(duration(400), "0.4s");
        assert_eq!(duration(42_000), "42s");
        assert_eq!(duration(125_000), "2m 5s");
        assert_eq!(duration(3_900_000), "1h 5m");
        assert_eq!(usd(0.004), "<$0.01");
        assert_eq!(usd(1.5), "$1.50");
        assert_eq!(tokens(1500), "1.5k");
        assert_eq!(tokens(25_000), "25k");
        assert_eq!(tokens(2_500_000), "2.5M");
        assert_eq!(pct(Some(0.456)), "46%");
        assert_eq!(pct(None), "–");
        assert_eq!(plural(1, "run"), "1 run");
        assert_eq!(plural(3, "run"), "3 runs");
    }

    #[test]
    fn conditions_say_what_they_check() {
        assert_eq!(op_label(Op::NotContains), "doesn't contain");
        assert!(!op_takes_value(Op::Exists));
        assert!(op_takes_value(Op::Equals));
        assert_eq!(text(&Value::String("a".into())), "a");
        assert_eq!(text(&serde_json::json!(3)), "3");
        assert_eq!(text(&Value::Null), "");
    }
}
