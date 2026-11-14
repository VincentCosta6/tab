//! What the Workflows tab's Insights and Factory views show: one
//! workflow's [`Stats`] for a window of days next to the window before it,
//! and every workflow's at once with hub-wide totals.
//!
//! Pure, like [`stats`](super::stats): the caller loads the runs (and,
//! for long windows, the [`rollup`](super::rollup)s) and passes `now` in.
//!
//! Short windows, up to [`RUN_DAYS`], break at the person's midnight and
//! are read from runs alone, so a 7-day window is today and the six days
//! before it, and the previous window is the seven days before that. Runs
//! are kept for 30 days, so a 30-day window's previous window is mostly
//! empty; it says so by having no runs.
//!
//! Longer windows (90 days, a year) break at **UTC** midnight, because
//! their older days come from daily rollups, which are by UTC day. Days
//! before the run log are read from rollups and the rest from runs
//! ([`rollup::summarize_span`]): counts, spend and the daily chart are
//! exact, step timing is approximate, and failure groups and the model
//! comparison cover only the run log (`detail_from` says from when). A
//! previous window that reaches past the 13 months of rollups is left
//! empty rather than compared against a part of itself.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use super::cap::CapUsage;
use super::rollup::{self, Rollup};
use super::stats::{Health, Stats, Window, health, summarize};
use super::{Run, Workflow};

/// The longest window: a year, from rollups.
pub const MAX_DAYS: u32 = 365;
/// The longest window read from runs alone, at the person's midnight.
pub const RUN_DAYS: u32 = 30;

/// The window of `days` days ending today (in the person's time, or UTC
/// for windows longer than [`RUN_DAYS`]), and the same number of days
/// before it.
pub fn windows(now: DateTime<Utc>, days: u32, utc_offset_minutes: i32) -> (Window, Window) {
    let days = days.clamp(1, MAX_DAYS);
    let utc_offset_minutes = if days > RUN_DAYS {
        0
    } else {
        utc_offset_minutes
    };
    let days = days as i64;
    let offset = Duration::minutes(utc_offset_minutes as i64);
    let today = (now + offset).date_naive();
    let midnight = (today - Duration::days(days - 1))
        .and_hms_opt(0, 0, 0)
        .expect("midnight exists")
        .and_utc()
        - offset;
    let current = Window {
        from: midnight,
        to: midnight + Duration::days(days),
        utc_offset_minutes,
    };
    let previous = Window {
        from: midnight - Duration::days(days),
        to: midnight,
        utc_offset_minutes,
    };
    (current, previous)
}

/// One workflow, this window and the one before.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkflowInsights {
    pub workflow_id: String,
    pub days: u32,
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    /// From its latest runs, whatever the window.
    pub health: Health,
    pub stats: Stats,
    pub previous: Stats,
    /// When set, the window reaches back past the run log: failure groups
    /// and models only cover runs from this time on, and step timing is
    /// approximate.
    #[serde(default)]
    pub detail_from: Option<DateTime<Utc>>,
    /// This month's spend against the workflow's caps, when it has any.
    #[serde(default)]
    pub cap: Option<CapUsage>,
}

/// Adds up one workflow's runs. `runs` may hold any runs; only this
/// workflow's count.
pub fn workflow_insights(
    workflow_id: &str,
    runs: &[Run],
    now: DateTime<Utc>,
    days: u32,
    utc_offset_minutes: i32,
) -> WorkflowInsights {
    workflow_insights_with(workflow_id, runs, &[], now, days, utc_offset_minutes)
}

/// [`workflow_insights`] with the rollups a long window reads.
pub fn workflow_insights_with(
    workflow_id: &str,
    runs: &[Run],
    rollups: &[Rollup],
    now: DateTime<Utc>,
    days: u32,
    utc_offset_minutes: i32,
) -> WorkflowInsights {
    let (current, previous) = windows(now, days, utc_offset_minutes);
    let mine: Vec<Run> = runs
        .iter()
        .filter(|r| r.workflow_id == workflow_id)
        .cloned()
        .collect();
    let my_rollups: Vec<Rollup> = rollups
        .iter()
        .filter(|r| r.workflow_id == workflow_id)
        .cloned()
        .collect();
    let span = Span::new(now, days);
    WorkflowInsights {
        workflow_id: workflow_id.to_string(),
        days: days.clamp(1, MAX_DAYS),
        from: current.from,
        to: current.to,
        health: health(&mine),
        stats: span.summarize(&mine, &my_rollups, &current),
        previous: span.previous(&mine, &my_rollups, &previous),
        detail_from: span.detail_from(&current),
        cap: None,
    }
}

/// How a window is read: from runs alone, or from rollups and runs.
struct Span {
    long: bool,
    split: DateTime<Utc>,
    keep_from: DateTime<Utc>,
}

impl Span {
    fn new(now: DateTime<Utc>, days: u32) -> Self {
        Self {
            long: days.clamp(1, MAX_DAYS) > RUN_DAYS,
            split: rollup::run_log_start(now),
            keep_from: rollup::day_start(rollup::keep_rollups_from(now)),
        }
    }

    fn summarize(&self, runs: &[Run], rollups: &[Rollup], w: &Window) -> Stats {
        if self.long {
            rollup::summarize_span(runs, rollups, w, self.split)
        } else {
            summarize(runs, w)
        }
    }

    fn previous(&self, runs: &[Run], rollups: &[Rollup], w: &Window) -> Stats {
        if self.long && w.from < self.keep_from {
            return Stats::default();
        }
        self.summarize(runs, rollups, w)
    }

    fn detail_from(&self, w: &Window) -> Option<DateTime<Utc>> {
        (self.long && w.from < self.split).then_some(self.split)
    }
}

/// One workflow's card on the factory floor.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FactoryWorkflow {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub version: u32,
    pub health: Health,
    pub stats: Stats,
    pub previous: Stats,
}

/// Every workflow at once.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Factory {
    pub days: u32,
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    /// Every workflow's runs added up. Run-level numbers only: steps,
    /// models, versions and failures belong to one workflow and are left
    /// empty.
    pub totals: Stats,
    pub previous_totals: Stats,
    /// What needs a look first: failing, then degraded, then healthy, then
    /// too new to say; within each, the lowest unattended success first.
    pub workflows: Vec<FactoryWorkflow>,
    /// As on [`WorkflowInsights::detail_from`].
    #[serde(default)]
    pub detail_from: Option<DateTime<Utc>>,
}

/// The factory overview over `workflows`. Runs of workflows not in the
/// list (deleted ones) are left out, so the cards add up to the totals.
pub fn factory(
    workflows: &[Workflow],
    runs: &[Run],
    now: DateTime<Utc>,
    days: u32,
    utc_offset_minutes: i32,
) -> Factory {
    factory_with(workflows, runs, &[], now, days, utc_offset_minutes)
}

/// [`factory`] with the rollups a long window reads.
pub fn factory_with(
    workflows: &[Workflow],
    runs: &[Run],
    rollups: &[Rollup],
    now: DateTime<Utc>,
    days: u32,
    utc_offset_minutes: i32,
) -> Factory {
    let (current, previous) = windows(now, days, utc_offset_minutes);
    let span = Span::new(now, days);
    let mut cards: Vec<FactoryWorkflow> = workflows
        .iter()
        .map(|w| {
            let mine: Vec<Run> = runs
                .iter()
                .filter(|r| r.workflow_id == w.id)
                .cloned()
                .collect();
            let my_rollups: Vec<Rollup> = rollups
                .iter()
                .filter(|r| r.workflow_id == w.id)
                .cloned()
                .collect();
            FactoryWorkflow {
                id: w.id.clone(),
                name: w.name.clone(),
                enabled: w.enabled,
                version: w.version,
                health: health(&mine),
                stats: span.summarize(&mine, &my_rollups, &current),
                previous: span.previous(&mine, &my_rollups, &previous),
            }
        })
        .collect();
    cards.sort_by(|a, b| {
        rank(a.health)
            .cmp(&rank(b.health))
            .then(
                a.stats
                    .counts
                    .unattended_rate()
                    .unwrap_or(1.0)
                    .total_cmp(&b.stats.counts.unattended_rate().unwrap_or(1.0)),
            )
            .then(b.stats.counts.runs.cmp(&a.stats.counts.runs))
            .then(a.name.cmp(&b.name))
    });
    let listed: Vec<Run> = runs
        .iter()
        .filter(|r| workflows.iter().any(|w| w.id == r.workflow_id))
        .cloned()
        .collect();
    let listed_rollups: Vec<Rollup> = rollups
        .iter()
        .filter(|r| workflows.iter().any(|w| w.id == r.workflow_id))
        .cloned()
        .collect();
    Factory {
        days: days.clamp(1, MAX_DAYS),
        from: current.from,
        to: current.to,
        totals: run_level(span.summarize(&listed, &listed_rollups, &current)),
        previous_totals: run_level(span.previous(&listed, &listed_rollups, &previous)),
        workflows: cards,
        detail_from: span.detail_from(&current),
    }
}

fn rank(h: Health) -> u8 {
    match h {
        Health::Failing => 0,
        Health::Degraded => 1,
        Health::Healthy => 2,
        Health::Unknown => 3,
    }
}

/// Drops what only makes sense for one workflow (steps are told apart by
/// id alone).
fn run_level(s: Stats) -> Stats {
    Stats {
        steps: Vec::new(),
        models: Vec::new(),
        versions: Vec::new(),
        failures: Vec::new(),
        ..s
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use serde_json::Value;

    use super::*;
    use crate::workflow::{RunStatus, StepRun, StepStatus, Trigger};

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 12, 15, 0, 0).unwrap()
    }

    fn wf(id: &str) -> Workflow {
        Workflow {
            id: id.into(),
            name: format!("Workflow {id}"),
            trigger: Trigger::Manual,
            ..Default::default()
        }
    }

    fn run(wf: &str, n: u32, status: RunStatus, at: DateTime<Utc>) -> Run {
        Run {
            id: format!("{wf}-{n}"),
            workflow_id: wf.into(),
            workflow_name: wf.into(),
            version: 1,
            status,
            dry_run: false,
            trigger: "t".into(),
            trigger_key: None,
            item_id: None,
            chain: vec![],
            steps: vec![StepRun {
                id: "s".into(),
                label: "S".into(),
                status: if status == RunStatus::Failed {
                    StepStatus::Failed
                } else {
                    StepStatus::Ok
                },
                input: Value::Null,
                output: Value::Null,
                error: (status == RunStatus::Failed).then(|| "boom 42".into()),
                ms: 10,
                metrics: None,
                interventions: vec![],
            }],
            error: None,
            started_at: at,
            finished_at: Some(at + Duration::seconds(5)),
            runner: String::new(),
            resume: None,
            rerun_of: None,
            started_by: None,
            caused_by: None,
            approved: Vec::new(),
        }
    }

    #[test]
    fn windows_break_at_the_persons_midnight() {
        // 15:00 UTC is 07:00 in UTC-8: today there began at 08:00 UTC.
        let (cur, prev) = windows(now(), 7, -480);
        assert_eq!(cur.to, Utc.with_ymd_and_hms(2026, 11, 13, 8, 0, 0).unwrap());
        assert_eq!(
            cur.from,
            Utc.with_ymd_and_hms(2026, 11, 6, 8, 0, 0).unwrap()
        );
        assert_eq!(
            (prev.from, prev.to),
            (cur.from - Duration::days(7), cur.from)
        );
        assert!(cur.from <= now() && now() < cur.to);
        // East of UTC the local day can already be tomorrow's.
        let (cur, _) = windows(
            Utc.with_ymd_and_hms(2026, 11, 12, 23, 0, 0).unwrap(),
            1,
            120,
        );
        assert_eq!(
            cur.from,
            Utc.with_ymd_and_hms(2026, 11, 12, 22, 0, 0).unwrap()
        );
    }

    #[test]
    fn days_are_clamped_to_a_year_and_long_windows_break_at_utc_midnight() {
        let (cur, _) = windows(now(), 0, 0);
        assert_eq!(cur.to - cur.from, Duration::days(1));
        assert_eq!(workflow_insights("a", &[], now(), 1000, 0).days, 365);
        for days in [90, 365] {
            let (cur, prev) = windows(now(), days, -480);
            assert_eq!(cur.to - cur.from, Duration::days(days as i64));
            assert_eq!(cur.to, Utc.with_ymd_and_hms(2026, 11, 13, 0, 0, 0).unwrap());
            assert_eq!((cur.utc_offset_minutes, prev.to), (0, cur.from));
        }
        // Short windows are always wholly inside the run log, whatever the
        // time zone, so they never need rollups.
        for offset in [-720, -480, 0, 330, 840] {
            for hour in [0, 11, 23] {
                let t = Utc.with_ymd_and_hms(2026, 11, 12, hour, 59, 0).unwrap();
                let (cur, _) = windows(t, RUN_DAYS, offset);
                assert!(cur.from >= rollup::run_log_start(t), "{offset} {hour}");
            }
        }
    }

    #[test]
    fn a_long_window_adds_rollups_to_runs_and_says_where_detail_starts() {
        let day = Duration::days(1);
        // Runs from 80 days back; only the last 30 days are still in the log.
        let all: Vec<Run> = (0..80)
            .map(|n| {
                let status = if n % 4 == 0 {
                    RunStatus::Failed
                } else {
                    RunStatus::Succeeded
                };
                run("a", n, status, now() - day * n as i32 - Duration::hours(1))
            })
            .collect();
        let rows = {
            let p = rollup::plan(now(), &all, &[]);
            p.put
        };
        let split = rollup::run_log_start(now());
        let log: Vec<Run> = all
            .iter()
            .filter(|r| r.started_at >= split)
            .cloned()
            .collect();

        let i = workflow_insights_with("a", &log, &rows, now(), 90, 0);
        assert_eq!((i.stats.counts.runs, i.stats.counts.failed), (80, 20));
        assert_eq!(i.stats.days.len(), 80);
        assert_eq!(i.detail_from, Some(split));
        // Failures only from the log.
        let in_log = log.iter().filter(|r| r.status == RunStatus::Failed).count() as u32;
        assert_eq!(i.stats.failures[0].count, in_log);
        // The 90 days before hold nothing, and that's not hidden.
        assert_eq!(i.previous.counts.runs, 0);
        // Without rollups, only the log counts.
        assert_eq!(
            workflow_insights_with("a", &log, &[], now(), 90, 0)
                .stats
                .counts
                .runs,
            log.len() as u32
        );
        // A year's previous window reaches past what rollups keep: empty.
        let y = workflow_insights_with("a", &log, &rows, now(), 365, 0);
        assert_eq!((y.stats.counts.runs, y.previous.counts.runs), (80, 0));
        // Short windows have no detail cut.
        assert_eq!(
            workflow_insights_with("a", &log, &rows, now(), 30, 0).detail_from,
            None
        );

        let f = factory_with(&[wf("a")], &log, &rows, now(), 90, 0);
        assert_eq!(f.totals.counts.runs, 80);
        assert_eq!(f.workflows[0].stats.counts.runs, 80);
        assert_eq!(f.detail_from, Some(split));
        assert!(f.totals.steps.is_empty());
    }

    #[test]
    fn insights_from_before_rollups_still_parse() {
        let i = workflow_insights("a", &[], now(), 7, 0);
        let mut v = serde_json::to_value(&i).unwrap();
        v.as_object_mut().unwrap().remove("detail_from");
        v.as_object_mut().unwrap().remove("cap");
        let back: WorkflowInsights = serde_json::from_value(v).unwrap();
        assert_eq!(back, i);
    }

    #[test]
    fn one_workflow_against_its_previous_window() {
        let day = Duration::days(1);
        let runs = vec![
            run("a", 1, RunStatus::Succeeded, now() - day),
            run("a", 2, RunStatus::Failed, now() - day * 2),
            run("a", 3, RunStatus::Succeeded, now() - day * 9),
            run("b", 1, RunStatus::Failed, now() - day),
        ];
        let i = workflow_insights("a", &runs, now(), 7, 0);
        assert_eq!((i.stats.counts.runs, i.stats.counts.failed), (2, 1));
        assert_eq!(
            (i.previous.counts.runs, i.previous.counts.succeeded),
            (1, 1)
        );
        assert_eq!(i.health, Health::Degraded, "2 of 3 latest runs worked");
        assert_eq!(i.stats.failures[0].error, "boom #");
    }

    #[test]
    fn the_factory_puts_what_is_not_working_first() {
        let hour = Duration::hours(1);
        let mut runs = Vec::new();
        for n in 0..10 {
            runs.push(run(
                "good",
                n,
                RunStatus::Succeeded,
                now() - hour * n as i32,
            ));
            let bad = if n < 7 {
                RunStatus::Failed
            } else {
                RunStatus::Succeeded
            };
            runs.push(run("bad", n, bad, now() - hour * n as i32));
            let meh = if n < 3 {
                RunStatus::Failed
            } else {
                RunStatus::Succeeded
            };
            runs.push(run("meh", n, meh, now() - hour * n as i32));
        }
        runs.push(run("new", 0, RunStatus::Succeeded, now() - hour));
        runs.push(run("gone", 0, RunStatus::Failed, now() - hour));
        let flows = [wf("good"), wf("new"), wf("meh"), wf("bad"), wf("idle")];
        let f = factory(&flows, &runs, now(), 7, 0);
        let order: Vec<(&str, Health)> = f
            .workflows
            .iter()
            .map(|w| (w.id.as_str(), w.health))
            .collect();
        assert_eq!(
            order,
            [
                ("bad", Health::Failing),
                ("meh", Health::Degraded),
                ("good", Health::Healthy),
                ("new", Health::Unknown),
                ("idle", Health::Unknown),
            ]
        );
        // The deleted workflow's run is not in the totals.
        assert_eq!(f.totals.counts.runs, 31);
        assert_eq!(f.totals.counts.failed, 10);
        assert!(f.totals.steps.is_empty() && f.totals.failures.is_empty());
        assert!(!f.totals.days.is_empty());
        assert_eq!(f.previous_totals.counts.runs, 0);
        assert_eq!(f.workflows[4].stats.counts.runs, 0);
    }
}
