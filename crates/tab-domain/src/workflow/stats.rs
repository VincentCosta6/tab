//! How a workflow is doing, added up from its runs: how often it finishes
//! on its own, how often a person steps in, what each step takes and costs,
//! and where it fails.
//!
//! Everything comes from [`summarize`], a pure function over run records,
//! so the desktop, the hub and a phone show the same numbers from the same
//! runs. Nothing here reads a clock or a store: the caller picks the runs
//! and the window.
//!
//! What counts:
//! * Dry runs never count.
//! * A run *did work* when it ended `succeeded`, `stopped` (a filter ended
//!   it, which isn't a failure), `failed` or `blocked`. Rates are over
//!   those. Runs still going, held back by a limit, or stopped as a loop
//!   are counted on their own and left out of the rates.
//! * A run the person *declined* at an approval ([`Run::declined`]) is
//!   neither a success nor a failure: it's counted on its own
//!   ([`Stats::declined`], and on its step) and left out of the rates.
//! * A run was *attended* when a person stepped in: any step records an
//!   [`Intervention`](super::Intervention), the run ended `blocked`, or it
//!   is a person's re-run of a failed run. It is *unplanned* unless every
//!   intervention was an approval the workflow asks for by design.
//! * *Unattended success* is a successful run nobody stepped in on.

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

use super::{Billing, InterventionKind, Run, RunStatus, StepRun, StepStatus, held_by_own_limit};

/// Runs to look at, by when they started: `from <= started_at < to`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Window {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    /// The person's offset from UTC, so days break at their midnight.
    pub utc_offset_minutes: i32,
}

/// The middle and the slow end of a set of times, in milliseconds.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Quantiles {
    /// How many values went in. Zero means the rest say nothing.
    pub n: u32,
    pub p50: u64,
    pub p95: u64,
}

impl Quantiles {
    /// Nearest-rank quantiles.
    pub fn of(mut values: Vec<u64>) -> Self {
        if values.is_empty() {
            return Self::default();
        }
        values.sort_unstable();
        let at = |q: f64| {
            let rank = (q * values.len() as f64).ceil() as usize;
            values[rank.clamp(1, values.len()) - 1]
        };
        Self {
            n: values.len() as u32,
            p50: at(0.50),
            p95: at(0.95),
        }
    }
}

/// What was spent, by how it was paid for: sessions on an API key cost
/// dollars; sessions on a subscription use up tokens and plan allowance,
/// and are never given a dollar figure.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Spend {
    /// Billed through API keys.
    pub cost_usd: f64,
    /// Tokens, however they were paid for.
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// The part of those tokens that ran on a subscription.
    pub plan_input_tokens: u64,
    pub plan_output_tokens: u64,
    /// Plan allowance used, as a sum of shares (0.5 is half a window).
    pub plan_share: f64,
}

impl Spend {
    pub(super) fn add(&mut self, sr: &StepRun) {
        let Some(m) = &sr.metrics else { return };
        self.input_tokens += m.input_tokens;
        self.output_tokens += m.output_tokens;
        if m.billing == Billing::Subscription {
            self.plan_input_tokens += m.input_tokens;
            self.plan_output_tokens += m.output_tokens;
            self.plan_share += m.plan_share;
        } else {
            self.cost_usd += m.cost_usd;
        }
    }
}

/// Run counts every breakdown shares.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Counts {
    /// Runs that did work. The denominator of every rate.
    pub runs: u32,
    /// Ended `succeeded` or `stopped`.
    pub succeeded: u32,
    /// Succeeded with nobody stepping in.
    pub unattended: u32,
    pub failed: u32,
    /// A person stepped in (whatever the outcome).
    pub attended: u32,
    /// Attended, and not only for approvals the workflow asks for.
    pub unplanned: u32,
}

impl Counts {
    fn rate(&self, n: u32) -> Option<f64> {
        (self.runs > 0).then(|| n as f64 / self.runs as f64)
    }
    pub fn success_rate(&self) -> Option<f64> {
        self.rate(self.succeeded)
    }
    pub fn unattended_rate(&self) -> Option<f64> {
        self.rate(self.unattended)
    }
    pub fn intervention_rate(&self) -> Option<f64> {
        self.rate(self.attended)
    }

    fn add(&mut self, run: &Run) {
        self.runs += 1;
        let ok = succeeded(run);
        let attended = attendance(run);
        if ok {
            self.succeeded += 1;
            if attended.is_none() {
                self.unattended += 1;
            }
        } else {
            self.failed += 1;
        }
        if let Some(planned) = attended {
            self.attended += 1;
            if !planned {
                self.unplanned += 1;
            }
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Stats {
    #[serde(flatten)]
    pub counts: Counts,
    /// Still running, or waiting on an agent.
    pub in_progress: u32,
    /// Refused by a rate limit or deferred to protect a provider's budget.
    pub held_back: u32,
    /// Stopped for starting inside their own chain.
    pub loop_stopped: u32,
    /// Of `held_back`, refused by the workflow's own runs-per-hour limit.
    /// Such refusals are written down at most once an hour, so this is
    /// about how many hours it hit its limit.
    #[serde(default)]
    pub held_by_limit: u32,
    /// The person said no at an approval, so the run ended there. Neither
    /// a success nor a failure.
    #[serde(default)]
    pub declined: u32,
    /// Start to finish, for runs that did work.
    pub duration_ms: Quantiles,
    pub spend: Spend,
    /// In the order the workflow runs them (as first seen, newest version
    /// first).
    pub steps: Vec<StepStats>,
    /// Agent steps by the model that ran them, costliest first.
    pub models: Vec<ModelStats>,
    /// Newest version first.
    pub versions: Vec<VersionStats>,
    /// Most frequent first.
    pub failures: Vec<FailureGroup>,
    /// Oldest day first; days with no runs are left out.
    pub days: Vec<DayStats>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct StepStats {
    pub id: String,
    pub label: String,
    /// Times it ran to an end (not skipped, dry or still waiting).
    pub runs: u32,
    pub ok: u32,
    pub failed: u32,
    pub blocked: u32,
    pub throttled: u32,
    /// Its `when` didn't hold.
    pub skipped: u32,
    /// Its whole time. For an agent step: queue plus work.
    pub ms: Quantiles,
    /// Agent steps only: waiting for a runner, the agent working, and the
    /// part of that spent waiting on the person.
    pub queue_ms: Quantiles,
    pub work_ms: Quantiles,
    pub human_wait_ms: Quantiles,
    pub spend: Spend,
    pub interventions: u32,
    pub unplanned_interventions: u32,
    /// Times a runner moved on to a family's next model.
    pub fallbacks: u32,
    /// Times the person OK'd it at an approval and it went ahead.
    #[serde(default)]
    pub approved: u32,
    /// Times the person said no to it at an approval (those runs are left
    /// out of everything else here).
    #[serde(default)]
    pub declined: u32,
}

impl StepStats {
    pub fn failure_rate(&self) -> Option<f64> {
        (self.runs > 0).then(|| self.failed as f64 / self.runs as f64)
    }
}

/// One agent step on one model: what picking that model got.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ModelStats {
    pub step_id: String,
    pub harness: String,
    pub model: String,
    /// The family that chose it, when one did.
    pub family: String,
    pub runs: u32,
    pub ok: u32,
    pub failed: u32,
    /// Finished with nobody stepping in on the step.
    pub unattended: u32,
    pub work_ms: Quantiles,
    pub spend: Spend,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct VersionStats {
    pub version: u32,
    #[serde(flatten)]
    pub counts: Counts,
    pub duration_ms: Quantiles,
    pub spend: Spend,
    /// Its first run in the window: where it shows on a trend.
    pub first_run: Option<DateTime<Utc>>,
}

/// Failures of one step with the same error, once ids and numbers are
/// taken out of it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FailureGroup {
    pub step_id: String,
    pub label: String,
    pub error: String,
    pub count: u32,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    /// The latest run it happened in.
    pub sample_run: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DayStats {
    pub day: NaiveDate,
    #[serde(flatten)]
    pub counts: Counts,
    pub duration_ms: Quantiles,
    pub spend: Spend,
}

/// A workflow's state at a glance, from its latest runs.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Health {
    /// Too few runs to say.
    Unknown,
    Healthy,
    Degraded,
    Failing,
}

/// Runs [`health`] looks at.
pub const HEALTH_RUNS: usize = 20;
/// Fewer runs than this say nothing.
pub const HEALTH_MIN_RUNS: usize = 3;

/// Health from the latest [`HEALTH_RUNS`] runs that did work: healthy at
/// 90% success and up, failing under half.
pub fn health(runs: &[Run]) -> Health {
    let mut latest: Vec<&Run> = runs.iter().filter(|r| did_work(r)).collect();
    latest.sort_by_key(|r| std::cmp::Reverse(r.started_at));
    latest.truncate(HEALTH_RUNS);
    if latest.len() < HEALTH_MIN_RUNS {
        return Health::Unknown;
    }
    let ok = latest.iter().filter(|r| succeeded(r)).count() as f64 / latest.len() as f64;
    if ok >= 0.9 {
        Health::Healthy
    } else if ok >= 0.5 {
        Health::Degraded
    } else {
        Health::Failing
    }
}

pub(super) fn did_work(run: &Run) -> bool {
    !run.dry_run
        && !run.declined()
        && matches!(
            run.status,
            RunStatus::Succeeded | RunStatus::Stopped | RunStatus::Failed | RunStatus::Blocked
        )
}

fn succeeded(run: &Run) -> bool {
    matches!(run.status, RunStatus::Succeeded | RunStatus::Stopped)
}

/// `None` when nobody stepped in; otherwise whether every intervention
/// was a planned one.
fn attendance(run: &Run) -> Option<bool> {
    let mut kinds: Vec<InterventionKind> = run
        .steps
        .iter()
        .flat_map(|s| s.interventions.iter().map(|i| i.kind))
        .collect();
    // Runs from before interventions were recorded still say they were
    // blocked on an approval.
    if run.status == RunStatus::Blocked && kinds.is_empty() {
        kinds.push(InterventionKind::Approval);
    }
    if run.rerun_of.is_some() {
        kinds.push(InterventionKind::Rerun);
    }
    (!kinds.is_empty()).then(|| kinds.iter().all(|k| k.planned()))
}

pub(super) fn duration_ms(run: &Run) -> Option<u64> {
    let ms = (run.finished_at? - run.started_at).num_milliseconds();
    Some(ms.max(0) as u64)
}

/// A step's whole time: for an agent step that reported, queue plus work
/// (its own `ms` only covers handing the job over).
pub(super) fn step_ms(sr: &StepRun) -> u64 {
    match &sr.metrics {
        Some(m) if m.queue_ms + m.work_ms > 0 => m.queue_ms + m.work_ms,
        _ => sr.ms,
    }
}

/// An error with what changes from run to run taken out, so the same
/// failure groups together: numbers and long hex ids become `#`, spaces
/// collapse, and it's cut at 160 characters.
pub fn normalize_error(error: &str) -> String {
    let mut out = String::new();
    let mut token = String::new();
    let flush = |token: &mut String, out: &mut String| {
        if token.is_empty() {
            return;
        }
        let has_digit = token.chars().any(|c| c.is_ascii_digit());
        if has_digit && token.len() >= 7 && token.chars().all(|c| c.is_ascii_hexdigit()) {
            out.push('#');
        } else if has_digit {
            let mut in_run = false;
            for c in token.chars() {
                if c.is_ascii_digit() {
                    if !in_run {
                        out.push('#');
                    }
                    in_run = true;
                } else {
                    out.push(c);
                    in_run = false;
                }
            }
        } else {
            out.push_str(token);
        }
        token.clear();
    };
    for c in error.chars() {
        if c.is_alphanumeric() {
            token.push(c);
        } else {
            flush(&mut token, &mut out);
            if c.is_whitespace() {
                if !out.ends_with(' ') {
                    out.push(' ');
                }
            } else {
                out.push(c);
            }
        }
    }
    flush(&mut token, &mut out);
    let out = out.trim();
    match out.char_indices().nth(160) {
        Some((cut, _)) => format!("{}…", &out[..cut]),
        None => out.to_string(),
    }
}

#[derive(Default)]
struct Group {
    counts: Counts,
    durations: Vec<u64>,
    spend: Spend,
    first_run: Option<DateTime<Utc>>,
}

impl Group {
    fn add(&mut self, run: &Run) {
        self.counts.add(run);
        self.durations.extend(duration_ms(run));
        for sr in &run.steps {
            self.spend.add(sr);
        }
        if self.first_run.is_none_or(|t| run.started_at < t) {
            self.first_run = Some(run.started_at);
        }
    }
}

#[derive(Default)]
struct StepAcc {
    stats: StepStats,
    ms: Vec<u64>,
    queue: Vec<u64>,
    work: Vec<u64>,
    human: Vec<u64>,
}

#[derive(Default)]
struct ModelAcc {
    stats: ModelStats,
    work: Vec<u64>,
}

/// The step's running totals, added on first sight.
fn step_acc<'a>(steps: &'a mut Vec<StepAcc>, sr: &StepRun) -> &'a mut StepAcc {
    let at = match steps.iter().position(|s| s.stats.id == sr.id) {
        Some(at) => at,
        None => {
            steps.push(StepAcc {
                stats: StepStats {
                    id: sr.id.clone(),
                    label: sr.label.clone(),
                    ..Default::default()
                },
                ..Default::default()
            });
            steps.len() - 1
        }
    };
    &mut steps[at]
}

/// Adds up the runs in `window`. Give it one workflow's runs: steps are
/// told apart by their id alone.
pub fn summarize(runs: &[Run], window: &Window) -> Stats {
    let mut stats = Stats::default();
    let mut all = Group::default();
    let mut versions: BTreeMap<u32, Group> = BTreeMap::new();
    let mut days: BTreeMap<NaiveDate, Group> = BTreeMap::new();
    let mut steps: Vec<StepAcc> = Vec::new();
    let mut models: Vec<ModelAcc> = Vec::new();
    let mut failures: Vec<FailureGroup> = Vec::new();

    // Newest first, so steps come out in the current version's order and a
    // failure group's label is its latest.
    let mut sorted: Vec<&Run> = runs
        .iter()
        .filter(|r| !r.dry_run && r.started_at >= window.from && r.started_at < window.to)
        .collect();
    sorted.sort_by(|a, b| b.started_at.cmp(&a.started_at).then(a.id.cmp(&b.id)));

    for run in sorted {
        match run.status {
            RunStatus::Running | RunStatus::Waiting => stats.in_progress += 1,
            RunStatus::Throttled => stats.held_back += 1,
            RunStatus::LoopStopped => stats.loop_stopped += 1,
            _ => {}
        }
        if held_by_own_limit(run) {
            stats.held_by_limit += 1;
        }
        if run.declined() {
            stats.declined += 1;
            if let Some(sr) = run.steps.last() {
                step_acc(&mut steps, sr).stats.declined += 1;
            }
            continue;
        }
        if !did_work(run) {
            continue;
        }
        all.add(run);
        versions.entry(run.version).or_default().add(run);
        let day =
            (run.started_at + Duration::minutes(window.utc_offset_minutes as i64)).date_naive();
        days.entry(day).or_default().add(run);

        for sr in &run.steps {
            let acc = step_acc(&mut steps, sr);
            let s = &mut acc.stats;
            s.interventions += sr.interventions.len() as u32;
            s.unplanned_interventions += sr
                .interventions
                .iter()
                .filter(|i| !i.kind.planned())
                .count() as u32;
            match sr.status {
                StepStatus::Skipped => {
                    s.skipped += 1;
                    continue;
                }
                StepStatus::Dry | StepStatus::Waiting => continue,
                StepStatus::Ok | StepStatus::Stopped => s.ok += 1,
                StepStatus::Failed => s.failed += 1,
                StepStatus::Blocked => s.blocked += 1,
                StepStatus::Throttled => s.throttled += 1,
            }
            s.runs += 1;
            if sr.status != StepStatus::Blocked
                && sr
                    .interventions
                    .iter()
                    .any(|i| i.kind == InterventionKind::Approval)
            {
                s.approved += 1;
            }
            s.spend.add(sr);
            acc.ms.push(step_ms(sr));

            if sr.status == StepStatus::Failed {
                let error = normalize_error(sr.error.as_deref().unwrap_or("Failed"));
                match failures
                    .iter_mut()
                    .find(|f| f.step_id == sr.id && f.error == error)
                {
                    Some(f) => {
                        f.count += 1;
                        f.first_seen = f.first_seen.min(run.started_at);
                    }
                    None => failures.push(FailureGroup {
                        step_id: sr.id.clone(),
                        label: sr.label.clone(),
                        error,
                        count: 1,
                        first_seen: run.started_at,
                        last_seen: run.started_at,
                        sample_run: run.id.clone(),
                    }),
                }
            }

            let Some(m) = &sr.metrics else { continue };
            s.fallbacks += m.fallbacks;
            acc.queue.push(m.queue_ms);
            acc.work.push(m.work_ms);
            acc.human.push(m.human_wait_ms);
            if m.harness.is_empty() && m.model.is_empty() {
                continue;
            }
            let at = match models.iter().position(|x| {
                x.stats.step_id == sr.id && x.stats.harness == m.harness && x.stats.model == m.model
            }) {
                Some(at) => at,
                None => {
                    models.push(ModelAcc {
                        stats: ModelStats {
                            step_id: sr.id.clone(),
                            harness: m.harness.clone(),
                            model: m.model.clone(),
                            family: m.family.clone(),
                            ..Default::default()
                        },
                        work: Vec::new(),
                    });
                    models.len() - 1
                }
            };
            let x = &mut models[at];
            x.stats.runs += 1;
            if sr.status == StepStatus::Failed {
                x.stats.failed += 1;
            } else {
                x.stats.ok += 1;
                if sr.interventions.is_empty() {
                    x.stats.unattended += 1;
                }
            }
            x.stats.spend.add(sr);
            x.work.push(m.work_ms);
        }
    }

    stats.counts = all.counts;
    stats.duration_ms = Quantiles::of(all.durations);
    stats.spend = all.spend;
    stats.steps = steps
        .into_iter()
        .map(|a| StepStats {
            ms: Quantiles::of(a.ms),
            queue_ms: Quantiles::of(a.queue),
            work_ms: Quantiles::of(a.work),
            human_wait_ms: Quantiles::of(a.human),
            ..a.stats
        })
        .collect();
    stats.models = models
        .into_iter()
        .map(|a| ModelStats {
            work_ms: Quantiles::of(a.work),
            ..a.stats
        })
        .collect();
    stats.models.sort_by(|a, b| {
        b.spend
            .cost_usd
            .total_cmp(&a.spend.cost_usd)
            .then(b.runs.cmp(&a.runs))
    });
    stats.versions = versions
        .into_iter()
        .rev()
        .map(|(version, g)| VersionStats {
            version,
            counts: g.counts,
            duration_ms: Quantiles::of(g.durations),
            spend: g.spend,
            first_run: g.first_run,
        })
        .collect();
    failures.sort_by(|a, b| b.count.cmp(&a.count).then(b.last_seen.cmp(&a.last_seen)));
    stats.failures = failures;
    stats.days = days
        .into_iter()
        .map(|(day, g)| DayStats {
            day,
            counts: g.counts,
            duration_ms: Quantiles::of(g.durations),
            spend: g.spend,
        })
        .collect();
    stats
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use serde_json::Value;

    use super::*;
    use crate::workflow::{Intervention, StepMetrics};

    fn t(day: u32, hour: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, day, hour, 0, 0).unwrap()
    }

    fn window() -> Window {
        Window {
            from: t(1, 0),
            to: t(30, 0),
            utc_offset_minutes: 0,
        }
    }

    fn step(id: &str, status: StepStatus, ms: u64) -> StepRun {
        StepRun {
            id: id.into(),
            label: format!("Step {id}"),
            status,
            input: Value::Null,
            output: Value::Null,
            error: None,
            ms,
            metrics: None,
            interventions: Vec::new(),
        }
    }

    fn agent(id: &str, status: StepStatus, model: &str, cost: f64, work_ms: u64) -> StepRun {
        StepRun {
            metrics: Some(StepMetrics {
                queue_ms: 1_000,
                work_ms,
                billing: Billing::Api,
                cost_usd: cost,
                input_tokens: 100,
                output_tokens: 10,
                family: "balanced".into(),
                harness: "claude_code".into(),
                model: model.into(),
                attempts: 1,
                ..Default::default()
            }),
            ..step(id, status, 5)
        }
    }

    fn run(id: &str, status: RunStatus, at: DateTime<Utc>, steps: Vec<StepRun>) -> Run {
        Run {
            id: id.into(),
            workflow_id: "w".into(),
            workflow_name: "W".into(),
            version: 1,
            status,
            dry_run: false,
            trigger: "t".into(),
            trigger_key: None,
            item_id: None,
            chain: vec![],
            steps,
            error: None,
            started_at: at,
            finished_at: Some(at + Duration::seconds(10)),
            runner: String::new(),
            resume: None,
            rerun_of: None,
            started_by: None,
            caused_by: None,
            approved: vec![],
        }
    }

    fn asked(mut sr: StepRun, kind: InterventionKind) -> StepRun {
        sr.interventions.push(Intervention { kind, at: t(2, 0) });
        sr
    }

    #[test]
    fn rates_are_over_runs_that_did_work() {
        let mut dry = run("dry", RunStatus::Succeeded, t(2, 1), vec![]);
        dry.dry_run = true;
        let mut going = run("going", RunStatus::Waiting, t(2, 2), vec![]);
        going.finished_at = None;
        let runs = vec![
            run("a", RunStatus::Succeeded, t(2, 3), vec![]),
            run("b", RunStatus::Stopped, t(2, 4), vec![]),
            run("c", RunStatus::Failed, t(2, 5), vec![]),
            run("d", RunStatus::Throttled, t(2, 6), vec![]),
            run("e", RunStatus::LoopStopped, t(2, 7), vec![]),
            run(
                "old",
                RunStatus::Failed,
                t(1, 0) - Duration::days(40),
                vec![],
            ),
            dry,
            going,
        ];
        let s = summarize(&runs, &window());
        assert_eq!(
            (s.counts.runs, s.counts.succeeded, s.counts.failed),
            (3, 2, 1)
        );
        assert_eq!((s.in_progress, s.held_back, s.loop_stopped), (1, 1, 1));
        assert_eq!(s.counts.unattended, 2);
        assert!((s.counts.success_rate().unwrap() - 2.0 / 3.0).abs() < 1e-9);
        assert_eq!(s.counts.intervention_rate(), Some(0.0));
        assert_eq!(
            s.duration_ms,
            Quantiles {
                n: 3,
                p50: 10_000,
                p95: 10_000
            }
        );
    }

    #[test]
    fn no_runs_means_no_rates() {
        let s = summarize(&[], &window());
        assert_eq!(s.counts.success_rate(), None);
        assert_eq!(s.counts.unattended_rate(), None);
        assert_eq!(s.duration_ms.n, 0);
        assert!(s.steps.is_empty() && s.days.is_empty());
    }

    #[test]
    fn a_person_stepping_in_makes_a_run_attended_and_approvals_are_planned() {
        let mut rerun = run("rerun", RunStatus::Succeeded, t(3, 5), vec![]);
        rerun.rerun_of = Some("failed".into());
        let runs = vec![
            run(
                "alone",
                RunStatus::Succeeded,
                t(3, 1),
                vec![step("a", StepStatus::Ok, 1)],
            ),
            run(
                "approved",
                RunStatus::Succeeded,
                t(3, 2),
                vec![asked(
                    step("a", StepStatus::Ok, 1),
                    InterventionKind::Approval,
                )],
            ),
            run(
                "question",
                RunStatus::Succeeded,
                t(3, 3),
                vec![asked(
                    asked(step("a", StepStatus::Ok, 1), InterventionKind::Approval),
                    InterventionKind::Question,
                )],
            ),
            // From before interventions were recorded.
            run(
                "blocked",
                RunStatus::Blocked,
                t(3, 4),
                vec![step("a", StepStatus::Blocked, 1)],
            ),
            rerun,
        ];
        let c = summarize(&runs, &window()).counts;
        assert_eq!((c.runs, c.succeeded, c.unattended), (5, 4, 1));
        assert_eq!((c.attended, c.unplanned), (4, 2));
        assert_eq!(c.unattended_rate(), Some(0.2));
        assert_eq!(c.intervention_rate(), Some(0.8));
    }

    #[test]
    fn steps_add_up_their_time_cost_and_outcomes() {
        let mut failed = step("post", StepStatus::Failed, 30);
        failed.error = Some("GitHub said 502".into());
        let runs = vec![
            run(
                "new",
                RunStatus::Succeeded,
                t(4, 2),
                vec![
                    agent("fix", StepStatus::Ok, "sonnet", 0.25, 60_000),
                    step("post", StepStatus::Ok, 10),
                ],
            ),
            run(
                "old",
                RunStatus::Failed,
                t(4, 1),
                vec![
                    asked(
                        agent("fix", StepStatus::Ok, "sonnet", 0.75, 120_000),
                        InterventionKind::Question,
                    ),
                    failed,
                    step("after", StepStatus::Skipped, 0),
                ],
            ),
        ];
        let s = summarize(&runs, &window());
        let ids: Vec<&str> = s.steps.iter().map(|x| x.id.as_str()).collect();
        assert_eq!(ids, ["fix", "post", "after"]);
        let fix = &s.steps[0];
        assert_eq!((fix.runs, fix.ok, fix.failed), (2, 2, 0));
        // An agent step's time is its queue and work, not the hand-over.
        assert_eq!(
            fix.ms,
            Quantiles {
                n: 2,
                p50: 61_000,
                p95: 121_000
            }
        );
        assert_eq!(fix.work_ms.p95, 120_000);
        assert_eq!(fix.queue_ms.p50, 1_000);
        assert!((fix.spend.cost_usd - 1.0).abs() < 1e-9);
        assert_eq!((fix.spend.input_tokens, fix.spend.output_tokens), (200, 20));
        assert_eq!((fix.interventions, fix.unplanned_interventions), (1, 1));
        let post = &s.steps[1];
        assert_eq!(
            (post.runs, post.failed, post.failure_rate()),
            (2, 1, Some(0.5))
        );
        assert_eq!(post.work_ms.n, 0);
        let after = &s.steps[2];
        assert_eq!(
            (after.runs, after.skipped, after.failure_rate()),
            (0, 1, None)
        );
        assert!((s.spend.cost_usd - 1.0).abs() < 1e-9);
    }

    #[test]
    fn models_compare_on_the_same_step() {
        // On a subscription: usage, never dollars, whatever the harness says.
        let mut on_plan = agent("fix", StepStatus::Ok, "haiku", 0.02, 30_000);
        let m = on_plan.metrics.as_mut().unwrap();
        m.billing = Billing::Subscription;
        m.plan_share = 0.04;
        let runs = vec![
            run(
                "a",
                RunStatus::Succeeded,
                t(5, 1),
                vec![agent("fix", StepStatus::Ok, "opus", 0.9, 90_000)],
            ),
            run(
                "b",
                RunStatus::Failed,
                t(5, 2),
                vec![agent("fix", StepStatus::Failed, "opus", 0.6, 50_000)],
            ),
            run("c", RunStatus::Succeeded, t(5, 3), vec![on_plan]),
            run(
                "d",
                RunStatus::Succeeded,
                t(5, 4),
                vec![asked(
                    agent("fix", StepStatus::Ok, "haiku", 0.03, 40_000),
                    InterventionKind::Question,
                )],
            ),
        ];
        let s = summarize(&runs, &window());
        assert_eq!(s.models.len(), 2);
        let (opus, haiku) = (&s.models[0], &s.models[1]);
        assert_eq!(
            (opus.model.as_str(), opus.runs, opus.ok, opus.failed),
            ("opus", 2, 1, 1)
        );
        assert!((opus.spend.cost_usd - 1.5).abs() < 1e-9 && opus.spend.plan_input_tokens == 0);
        assert_eq!((haiku.runs, haiku.ok, haiku.unattended), (2, 2, 1));
        assert_eq!(haiku.family, "balanced");
        assert!((haiku.spend.cost_usd - 0.03).abs() < 1e-9);
        assert_eq!(
            (haiku.spend.input_tokens, haiku.spend.plan_input_tokens),
            (200, 100)
        );
        assert!((haiku.spend.plan_share - 0.04).abs() < 1e-9);
        assert!((s.spend.cost_usd - 1.53).abs() < 1e-9 && s.spend.plan_output_tokens == 10);
        assert_eq!(haiku.work_ms.p50, 30_000);
    }

    #[test]
    fn versions_and_days_break_the_same_runs_down() {
        let mut v2 = run("v2", RunStatus::Failed, t(7, 1), vec![]);
        v2.version = 2;
        let runs = vec![
            run("late", RunStatus::Succeeded, t(6, 23), vec![]),
            run("early", RunStatus::Succeeded, t(6, 3), vec![]),
            v2,
        ];
        let s = summarize(&runs, &window());
        let v: Vec<(u32, u32, u32)> = s
            .versions
            .iter()
            .map(|v| (v.version, v.counts.runs, v.counts.succeeded))
            .collect();
        assert_eq!(v, [(2, 1, 0), (1, 2, 2)]);
        assert_eq!(s.versions[1].first_run, Some(t(6, 3)));
        let d: Vec<(u32, u32)> = s
            .days
            .iter()
            .map(|d| (d.day.to_string()[8..].parse().unwrap(), d.counts.runs))
            .collect();
        assert_eq!(d, [(6, 2), (7, 1)]);

        // Two hours ahead of UTC, 23:00 on the 6th is already the 7th.
        let ahead = Window {
            utc_offset_minutes: 120,
            ..window()
        };
        let d: Vec<u32> = summarize(&runs, &ahead)
            .days
            .iter()
            .map(|d| d.counts.runs)
            .collect();
        assert_eq!(d, [1, 2]);
    }

    #[test]
    fn failures_group_by_step_and_error_without_the_ids() {
        let fail = |id: &str, at, step_id: &str, error: &str| {
            let mut sr = step(step_id, StepStatus::Failed, 1);
            sr.error = Some(error.into());
            run(id, RunStatus::Failed, at, vec![sr])
        };
        let runs = vec![
            fail(
                "r1",
                t(8, 1),
                "merge",
                "PR 4411 isn't mergeable (sha 9f8e7d6c5b4a)",
            ),
            fail(
                "r2",
                t(8, 3),
                "merge",
                "PR 52 isn't mergeable (sha 0a1b2c3d4e5f)",
            ),
            fail("r3", t(8, 2), "merge", "Timed out after 60 s"),
            fail("r4", t(8, 4), "notify", "Timed out after 60 s"),
        ];
        let f = summarize(&runs, &window()).failures;
        assert_eq!(f.len(), 3);
        assert_eq!(f[0].error, "PR # isn't mergeable (sha #)");
        assert_eq!((f[0].count, f[0].sample_run.as_str()), (2, "r2"));
        assert_eq!((f[0].first_seen, f[0].last_seen), (t(8, 1), t(8, 3)));
        assert_eq!(
            (f[1].step_id.as_str(), f[2].step_id.as_str()),
            ("notify", "merge")
        );
    }

    #[test]
    fn errors_lose_what_changes_between_runs() {
        assert_eq!(
            normalize_error("HTTP 502  from\napi.github.com"),
            "HTTP # from api.github.com"
        );
        assert_eq!(
            normalize_error("job 3f2a9c1e-5b7d-4e8a failed"),
            "job #-#b#d-#e#a failed"
        );
        assert_eq!(
            normalize_error("deadbeef is a word, v2 isn't"),
            "deadbeef is a word, v# isn't"
        );
        assert_eq!(normalize_error(&"x".repeat(200)).chars().count(), 161);
        assert_eq!(normalize_error(""), "");
    }

    #[test]
    fn health_reads_the_latest_runs() {
        let mk = |n: u32, ok: bool| {
            run(
                &format!("r{n}"),
                if ok {
                    RunStatus::Succeeded
                } else {
                    RunStatus::Failed
                },
                t(1, 0) + Duration::minutes(n as i64),
                vec![],
            )
        };
        assert_eq!(health(&[mk(1, true), mk(2, true)]), Health::Unknown);
        let good: Vec<Run> = (0..20).map(|n| mk(n, true)).collect();
        assert_eq!(health(&good), Health::Healthy);
        let mut mixed = good.clone();
        mixed.extend((20..25).map(|n| mk(n, false)));
        assert_eq!(health(&mixed), Health::Degraded);
        // Old successes don't hide a workflow that now fails every time.
        mixed.extend((25..40).map(|n| mk(n, false)));
        assert_eq!(health(&mixed), Health::Failing);
        // Held-back runs say nothing about health.
        let mut held = good;
        held.extend((20..60).map(|n| {
            let mut r = mk(n, false);
            r.status = RunStatus::Throttled;
            r
        }));
        assert_eq!(health(&held), Health::Healthy);
    }

    #[test]
    fn stats_round_trip_as_json_with_counts_at_the_top() {
        let runs = vec![run(
            "a",
            RunStatus::Succeeded,
            t(9, 1),
            vec![agent("fix", StepStatus::Ok, "sonnet", 0.1, 5)],
        )];
        let s = summarize(&runs, &window());
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["runs"], 1);
        assert_eq!(v["days"][0]["day"], "2026-11-09");
        assert_eq!(serde_json::from_value::<Stats>(v).unwrap(), s);
    }

    #[test]
    fn a_declined_run_is_neither_a_success_nor_a_failure() {
        let mut no = asked(
            step("merge", StepStatus::Blocked, 0),
            InterventionKind::Approval,
        );
        no.error = Some("You declined this step.".into());
        let yes = asked(step("merge", StepStatus::Ok, 4), InterventionKind::Approval);
        let runs = vec![
            run(
                "declined",
                RunStatus::Stopped,
                t(10, 1),
                vec![step("pr", StepStatus::Ok, 1), no],
            ),
            run(
                "approved",
                RunStatus::Succeeded,
                t(10, 2),
                vec![step("pr", StepStatus::Ok, 1), yes],
            ),
            run(
                "filtered",
                RunStatus::Stopped,
                t(10, 3),
                vec![
                    step("pr", StepStatus::Ok, 1),
                    step("small", StepStatus::Stopped, 1),
                ],
            ),
        ];
        let s = summarize(&runs, &window());
        assert_eq!(s.declined, 1);
        assert_eq!(
            (s.counts.runs, s.counts.succeeded, s.counts.failed),
            (2, 2, 0)
        );
        assert_eq!((s.counts.attended, s.counts.unplanned), (1, 0));
        let merge = s.steps.iter().find(|x| x.id == "merge").unwrap();
        assert_eq!((merge.runs, merge.approved, merge.declined), (1, 1, 1));
        let pr = s.steps.iter().find(|x| x.id == "pr").unwrap();
        assert_eq!(pr.runs, 2, "the declined run's steps don't count");
        assert_eq!(s.days.iter().map(|d| d.counts.runs).sum::<u32>(), 2);
        // Nor does it move health.
        let mut all_declined: Vec<Run> = (0..5)
            .map(|n| {
                let mut r = runs[0].clone();
                r.id = format!("d{n}");
                r
            })
            .collect();
        assert_eq!(health(&all_declined), Health::Unknown);
        all_declined.extend((0..3).map(|n| {
            let mut r = runs[1].clone();
            r.id = format!("a{n}");
            r
        }));
        assert_eq!(health(&all_declined), Health::Healthy);
    }

    #[test]
    fn held_back_by_its_own_limit_is_told_apart_from_other_holds() {
        let mut own = run("own", RunStatus::Throttled, t(11, 1), vec![]);
        own.error = Some(crate::workflow::own_limit_error(30));
        let mut all = run("all", RunStatus::Throttled, t(11, 2), vec![]);
        all.error =
            Some("All workflows together hit 200 runs an hour; this one was skipped.".into());
        let s = summarize(&[own, all], &window());
        assert_eq!((s.held_back, s.held_by_limit), (2, 1));
    }

    #[test]
    fn stats_written_before_declines_were_counted_still_parse() {
        let mut v = serde_json::to_value(summarize(
            &[run(
                "a",
                RunStatus::Succeeded,
                t(9, 1),
                vec![step("a", StepStatus::Ok, 1)],
            )],
            &window(),
        ))
        .unwrap();
        let o = v.as_object_mut().unwrap();
        o.remove("declined");
        o.remove("held_by_limit");
        for st in o["steps"].as_array_mut().unwrap() {
            let st = st.as_object_mut().unwrap();
            st.remove("approved");
            st.remove("declined");
        }
        let s: Stats = serde_json::from_value(v).unwrap();
        assert_eq!(
            (s.declined, s.held_by_limit, s.steps[0].approved),
            (0, 0, 0)
        );
    }

    #[test]
    fn old_run_records_still_parse() {
        let old = r#"{"id":"r","workflow_id":"w","workflow_name":"W","version":1,"status":"succeeded","dry_run":false,"trigger":"t","trigger_key":null,"item_id":null,"chain":[],"steps":[{"id":"a","label":"A","status":"ok","error":null,"ms":3}],"error":null,"started_at":"2026-11-02T00:00:00Z","finished_at":null}"#;
        let run: Run = serde_json::from_str(old).unwrap();
        assert!(run.steps[0].metrics.is_none() && run.steps[0].interventions.is_empty());
        assert!(run.rerun_of.is_none());
        // And what's written for a step with nothing new is what it was.
        let v = serde_json::to_value(&run).unwrap();
        assert!(v["steps"][0].get("metrics").is_none() && v.get("rerun_of").is_none());
    }
}
