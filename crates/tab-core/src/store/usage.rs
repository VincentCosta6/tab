//! Agent spend and Claude plan limits.

use chrono::{DateTime, Datelike, Local, TimeZone, Utc};
use rusqlite::params;
use serde::Serialize;

use super::{Store, parse_ts, ts};

/// One agent turn or ranking call.
#[derive(Debug, Clone, Default)]
pub struct NewUsage {
    pub purpose: &'static str,
    pub session_id: Option<String>,
    pub agent: String,
    pub model: Option<String>,
    pub billing: &'static str,
    pub cost_usd: f64,
    pub input_tokens: i64,
    pub output_tokens: i64,
}

/// A Claude plan window such as `five_hour` or `seven_day`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PlanWindow {
    pub window: String,
    /// 0.0 to 1.0, when Claude Code reports it.
    pub utilization: Option<f64>,
    pub resets_at: Option<DateTime<Utc>>,
    pub status: String,
    pub updated_at: DateTime<Utc>,
}

/// One stored turn's spend, as a chat's runner reads it back.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnUsage {
    pub at: DateTime<Utc>,
    pub agent: String,
    pub model: Option<String>,
    /// 'subscription', 'api' or 'unknown'.
    pub billing: String,
    pub cost_usd: f64,
    pub input_tokens: i64,
    pub output_tokens: i64,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct UsageSummary {
    pub today_usd: f64,
    pub month_usd: f64,
    pub today_agent_usd: f64,
    pub today_ranker_usd: f64,
    pub today_turns: i64,
    pub today_tokens: i64,
    /// Billing of the most recent call: 'subscription', 'api' or 'unknown'.
    pub billing: Option<String>,
    pub windows: Vec<PlanWindow>,
}

/// Midnight today and the 1st of this month, in local time, as UTC.
fn period_starts(now: DateTime<Utc>) -> (DateTime<Utc>, DateTime<Utc>) {
    let local = now.with_timezone(&Local);
    let day = Local
        .with_ymd_and_hms(local.year(), local.month(), local.day(), 0, 0, 0)
        .earliest()
        .unwrap_or(local);
    let month = Local
        .with_ymd_and_hms(local.year(), local.month(), 1, 0, 0, 0)
        .earliest()
        .unwrap_or(local);
    (day.with_timezone(&Utc), month.with_timezone(&Utc))
}

impl Store {
    pub fn usage_add(&self, u: &NewUsage, now: DateTime<Utc>) -> anyhow::Result<()> {
        self.conn().execute(
            "INSERT INTO agent_usage (at, purpose, session_id, agent, model, billing, cost_usd, input_tokens, output_tokens)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                ts(now), u.purpose, u.session_id, u.agent, u.model, u.billing, u.cost_usd,
                u.input_tokens, u.output_tokens
            ],
        )?;
        Ok(())
    }

    /// What one chat spent, oldest turn first.
    pub fn usage_for_session(&self, session_id: &str) -> anyhow::Result<Vec<TurnUsage>> {
        let conn = self.conn();
        let rows = conn
            .prepare(
                "SELECT at, agent, model, billing, cost_usd, input_tokens, output_tokens
                 FROM agent_usage WHERE session_id = ?1 ORDER BY id",
            )?
            .query_map([session_id], |r| {
                Ok(TurnUsage {
                    at: parse_ts(&r.get::<_, String>(0)?)?,
                    agent: r.get(1)?,
                    model: r.get(2)?,
                    billing: r.get(3)?,
                    cost_usd: r.get(4)?,
                    input_tokens: r.get(5)?,
                    output_tokens: r.get(6)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// Whether anything but chat `session_id` used the Claude plan since
    /// `since` (another chat, or the ranker).
    pub fn plan_used_elsewhere(
        &self,
        session_id: &str,
        since: DateTime<Utc>,
    ) -> anyhow::Result<bool> {
        let n: i64 = self.conn().query_row(
            "SELECT COUNT(*) FROM agent_usage
             WHERE billing = 'subscription' AND at >= ?1 AND session_id IS NOT ?2",
            params![ts(since), session_id],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    pub fn plan_window_set(&self, w: &PlanWindow) -> anyhow::Result<()> {
        self.conn().execute(
            "INSERT INTO plan_limits (window, utilization, resets_at, status, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (window) DO UPDATE SET utilization = excluded.utilization, resets_at = excluded.resets_at,
                 status = excluded.status, updated_at = excluded.updated_at",
            params![w.window, w.utilization, w.resets_at.map(ts), w.status, ts(w.updated_at)],
        )?;
        Ok(())
    }

    pub fn plan_windows(&self) -> anyhow::Result<Vec<PlanWindow>> {
        let conn = self.conn();
        let rows = conn
            .prepare("SELECT window, utilization, resets_at, status, updated_at FROM plan_limits ORDER BY window")?
            .query_map([], |r| {
                Ok(PlanWindow {
                    window: r.get(0)?,
                    utilization: r.get(1)?,
                    resets_at: r.get::<_, Option<String>>(2)?.map(|s| parse_ts(&s)).transpose()?,
                    status: r.get(3)?,
                    updated_at: parse_ts(&r.get::<_, String>(4)?)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    pub fn usage_summary(&self, now: DateTime<Utc>) -> anyhow::Result<UsageSummary> {
        let (day, month) = period_starts(now);
        let mut s = {
            let conn = self.conn();
            let (today_usd, today_agent_usd, today_ranker_usd, today_turns, today_tokens) = conn
                .query_row(
                    "SELECT COALESCE(SUM(cost_usd), 0),
                            COALESCE(SUM(CASE WHEN purpose = 'agent' THEN cost_usd END), 0),
                            COALESCE(SUM(CASE WHEN purpose = 'ranker' THEN cost_usd END), 0),
                            COUNT(CASE WHEN purpose = 'agent' THEN 1 END),
                            COALESCE(SUM(input_tokens + output_tokens), 0)
                     FROM agent_usage WHERE at >= ?1",
                    [ts(day)],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
                )?;
            let month_usd = conn.query_row(
                "SELECT COALESCE(SUM(cost_usd), 0) FROM agent_usage WHERE at >= ?1",
                [ts(month)],
                |r| r.get(0),
            )?;
            let billing = conn
                .query_row(
                    "SELECT billing FROM agent_usage ORDER BY id DESC LIMIT 1",
                    [],
                    |r| r.get(0),
                )
                .ok();
            UsageSummary {
                today_usd,
                month_usd,
                today_agent_usd,
                today_ranker_usd,
                today_turns,
                today_tokens,
                billing,
                windows: Vec::new(),
            }
        };
        s.windows = self.plan_windows()?;
        Ok(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sums_today_and_keeps_latest_plan_window() {
        let s = Store::in_memory().unwrap();
        let now = Utc::now();
        for (purpose, cost) in [("agent", 0.5), ("ranker", 0.01), ("agent", 0.25)] {
            s.usage_add(
                &NewUsage {
                    purpose,
                    agent: "claude_code".into(),
                    billing: "subscription",
                    cost_usd: cost,
                    ..Default::default()
                },
                now,
            )
            .unwrap();
        }
        let sum = s.usage_summary(now).unwrap();
        assert!((sum.today_usd - 0.76).abs() < 1e-9);
        assert!((sum.today_agent_usd - 0.75).abs() < 1e-9);
        assert_eq!(sum.today_turns, 2);
        assert_eq!(sum.billing.as_deref(), Some("subscription"));

        for u in [0.5, 0.81] {
            s.plan_window_set(&PlanWindow {
                window: "seven_day".into(),
                utilization: Some(u),
                resets_at: None,
                status: "allowed".into(),
                updated_at: now,
            })
            .unwrap();
        }
        assert_eq!(s.plan_windows().unwrap()[0].utilization, Some(0.81));
    }

    #[test]
    fn a_chats_turns_read_back_in_order_and_other_plan_use_shows() {
        let s = Store::in_memory().unwrap();
        let t0 = Utc::now() - chrono::Duration::minutes(10);
        let add = |session: Option<&str>, billing: &'static str, cost: f64, at| {
            s.usage_add(
                &NewUsage {
                    purpose: "agent",
                    session_id: session.map(str::to_string),
                    agent: "claude_code".into(),
                    model: Some("claude-opus-5-5".into()),
                    billing,
                    cost_usd: cost,
                    input_tokens: 100,
                    output_tokens: 10,
                },
                at,
            )
            .unwrap()
        };
        add(Some("job"), "subscription", 0.5, t0);
        add(
            Some("job"),
            "subscription",
            0.25,
            t0 + chrono::Duration::minutes(1),
        );
        add(Some("other"), "api", 1.0, t0 + chrono::Duration::minutes(2));
        let rows = s.usage_for_session("job").unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].cost_usd, 0.25);
        assert_eq!(rows[0].billing, "subscription");
        // An API-key chat doesn't touch the plan.
        assert!(!s.plan_used_elsewhere("job", t0).unwrap());
        // The ranker on the plan does.
        add(
            None,
            "subscription",
            0.01,
            t0 + chrono::Duration::minutes(3),
        );
        assert!(s.plan_used_elsewhere("job", t0).unwrap());
        assert!(
            !s.plan_used_elsewhere("job", t0 + chrono::Duration::minutes(4))
                .unwrap()
        );
    }
}
