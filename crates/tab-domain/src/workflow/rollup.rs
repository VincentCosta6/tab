//! Daily rollups: what a workflow's runs added up to on one day, kept for
//! 13 months so trends outlive the 30-day run log.
//!
//! A rollup is keyed by workflow, version and **UTC day** (`2026-11-12`
//! means 00:00 to 24:00 UTC), so a day means the same thing on every
//! device whatever its time zone. Views may label the days in the person's
//! time; the numbers belong to UTC days.
//!
//! # What a rollup holds
//!
//! [`DayRollup`] is the day's [`Counts`], the runs that did no work (still
//! going, held back, loop-stopped), [`Spend`] (API dollars and subscription
//! usage kept apart, as everywhere), each step's counts and spend, and
//! timing in a form that can be merged ([`Timing`]). Counts and spend add
//! up exactly, so a 90-day window's counts, spend and daily chart are
//! exact. Medians can't be added up, so timing is kept as a small
//! histogram and quantiles are read back from the merged histogram; see
//! [`Timing`] for how close that is.
//!
//! # When rollups are written, and why devices sharing a hub agree
//!
//! A rollup is a pure function of the day's runs ([`roll_up`]): the same
//! runs give the same body, byte for byte, on every device (runs are added
//! up in a fixed order, timing is integer-only). Runs sync through the hub,
//! so every device holds the same runs and computes the same rows.
//! [`plan`] runs on each device's maintenance pass (hourly), *before* the
//! run log is pruned:
//!
//! * Every complete UTC day still wholly in the run log (from
//!   [`run_log_start`] to yesterday) is rolled up again from its runs, and
//!   written only when the body differs from the stored one. Two devices
//!   rolling up the same day write identical rows, and rolling up twice
//!   changes nothing, so there is no sync churn and nothing to fight over:
//!   whichever write the hub orders last, it holds the same body. A device
//!   that briefly holds fewer runs (sync lag) may write a smaller body, but
//!   it is rolled up again on the next pass once the runs arrive, so all
//!   devices converge while the day is in the log.
//! * The run log is pruned at a UTC midnight ([`run_log_start`]), whole
//!   days at a time, and only after the same pass rolled the day up. A day
//!   older than that is **frozen**: its rollup is never rewritten, because
//!   its runs may be partly pruned and a recount would undercount. A day
//!   with runs but no rollup yet (this device was off for a while and never
//!   pruned it) is rolled up once, from all of its runs.
//! * Today is never rolled up: views read today from runs.
//!
//! **Late runs.** A run that reaches this device while its day is still in
//! the run log (another device was offline for a few days) is counted: the
//! next pass rolls its day up again. A run that arrives after its day was
//! frozen is not counted in the rollup and is pruned with the log; a run
//! still going when its day freezes stays counted as in progress. Both
//! need a device offline for about a month.
//!
//! # Reading a long window
//!
//! [`summarize_span`] reads days before [`run_log_start`] from rollups and
//! the days after from runs, never both for the same day. The recent part
//! is rolled up the same way, so timing is merged from one form; failure
//! groups and the model comparison come from the runs part only.

use std::collections::BTreeMap;

use chrono::{DateTime, Datelike, Duration, Months, NaiveDate, Utc};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::stats::{
    Counts, DayStats, Quantiles, Spend, Stats, StepStats, VersionStats, Window, did_work,
    duration_ms, step_ms, summarize,
};
use super::{Run, StepStatus};

/// Whole UTC days of runs kept before today. Runs that started before
/// [`run_log_start`] are pruned.
pub const RUN_LOG_DAYS: i64 = 30;
/// How long rollups are kept.
pub const ROLLUP_MONTHS: u32 = 13;

/// 00:00 UTC on `day`.
pub fn day_start(day: NaiveDate) -> DateTime<Utc> {
    day.and_hms_opt(0, 0, 0).expect("midnight exists").and_utc()
}

/// Where the run log starts: 00:00 UTC [`RUN_LOG_DAYS`] days before today.
/// Runs before it are pruned; days from it on are read from runs.
pub fn run_log_start(now: DateTime<Utc>) -> DateTime<Utc> {
    day_start(now.date_naive() - Duration::days(RUN_LOG_DAYS))
}

/// The oldest UTC day whose rollups are kept.
pub fn keep_rollups_from(now: DateTime<Utc>) -> NaiveDate {
    let today = now.date_naive();
    today
        .checked_sub_months(Months::new(ROLLUP_MONTHS))
        .unwrap_or(today)
}

/// The first day of `now`'s UTC month.
pub fn month_start(now: DateTime<Utc>) -> NaiveDate {
    now.date_naive()
        .with_day(1)
        .expect("every month has a first")
}

// ---------------------------------------------------------------------------
// Timing
// ---------------------------------------------------------------------------

/// Times in a form that adds up: a count, the sum and the largest, and a
/// sparse histogram over fixed log-linear buckets.
///
/// Why this form: quantiles can't be merged, and count, sum and max alone
/// give a mean but no median. A fixed histogram merges by adding bucket
/// counts, which is exact, associative and order-free, so any days merge
/// to the same result. The buckets are integer-only (no floating-point
/// log), so every device puts a time in the same bucket. Below 8 ms each
/// millisecond is its own bucket; above, each power of two is split into
/// four equal buckets (8, 10, 12, 14, 16, 20, … ms). A quantile is read as
/// its bucket's midpoint (capped at the largest time seen), within 12.5%
/// of the true nearest-rank value; that is what a 90-day median is.
/// A run of times spread over seconds to hours uses a few dozen buckets,
/// and only the non-empty ones are stored.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Timing {
    pub n: u32,
    pub sum_ms: u64,
    pub max_ms: u64,
    /// `[bucket, count]` for the non-empty buckets, by bucket.
    pub buckets: Vec<[u32; 2]>,
}

impl Timing {
    /// The bucket `ms` falls in.
    pub fn bucket(ms: u64) -> u32 {
        if ms < 8 {
            return ms as u32;
        }
        let k = 63 - ms.leading_zeros();
        let sub = ((ms >> (k - 2)) & 3) as u32;
        8 + (k - 3) * 4 + sub
    }

    /// A bucket's range, `lo <= ms < hi`.
    pub fn bounds(bucket: u32) -> (u64, u64) {
        if bucket < 8 {
            return (bucket as u64, bucket as u64 + 1);
        }
        let k = 3 + (bucket - 8) / 4;
        let sub = ((bucket - 8) % 4) as u128;
        let at = |m: u128| u64::try_from(m << (k - 2)).unwrap_or(u64::MAX);
        (at(4 + sub), at(5 + sub))
    }

    pub fn of(values: impl IntoIterator<Item = u64>) -> Self {
        let mut t = Self::default();
        for v in values {
            t.add(v);
        }
        t
    }

    pub fn add(&mut self, ms: u64) {
        self.n += 1;
        self.sum_ms = self.sum_ms.saturating_add(ms);
        self.max_ms = self.max_ms.max(ms);
        self.put(Self::bucket(ms), 1);
    }

    fn put(&mut self, bucket: u32, count: u32) {
        match self.buckets.binary_search_by_key(&bucket, |b| b[0]) {
            Ok(at) => self.buckets[at][1] += count,
            Err(at) => self.buckets.insert(at, [bucket, count]),
        }
    }

    pub fn merge(&mut self, other: &Timing) {
        self.n += other.n;
        self.sum_ms = self.sum_ms.saturating_add(other.sum_ms);
        self.max_ms = self.max_ms.max(other.max_ms);
        for [b, c] in &other.buckets {
            self.put(*b, *c);
        }
    }

    /// Nearest-rank quantile `q` (0 to 1), as its bucket's midpoint (the
    /// top rank is the largest time, exactly).
    pub fn quantile(&self, q: f64) -> u64 {
        if self.n == 0 {
            return 0;
        }
        let rank = ((q * self.n as f64).ceil() as u32).clamp(1, self.n);
        if rank == self.n {
            return self.max_ms; // the largest is kept exactly
        }
        let mut seen = 0;
        for [b, c] in &self.buckets {
            seen += c;
            if seen >= rank {
                let (lo, hi) = Self::bounds(*b);
                return (lo + (hi - lo) / 2).min(self.max_ms);
            }
        }
        self.max_ms
    }

    pub fn quantiles(&self) -> Quantiles {
        Quantiles {
            n: self.n,
            p50: self.quantile(0.50),
            p95: self.quantile(0.95),
        }
    }
}

// ---------------------------------------------------------------------------
// The rollup
// ---------------------------------------------------------------------------

/// One step on one day. Its counts and spend are [`StepStats`]'s; its times
/// are [`Timing`]s.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct StepRollup {
    pub id: String,
    pub label: String,
    pub runs: u32,
    pub ok: u32,
    pub failed: u32,
    pub blocked: u32,
    pub throttled: u32,
    pub skipped: u32,
    pub ms: Timing,
    pub queue_ms: Timing,
    pub work_ms: Timing,
    pub human_wait_ms: Timing,
    pub spend: Spend,
    pub interventions: u32,
    pub unplanned_interventions: u32,
    pub fallbacks: u32,
}

/// What one version of a workflow's runs added up to on one UTC day. Every
/// field is optional when read, so rows from older and newer apps parse.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct DayRollup {
    /// Runs that did work, as [`Stats`] counts them.
    pub counts: Counts,
    pub in_progress: u32,
    pub held_back: u32,
    pub loop_stopped: u32,
    /// Start to finish, for runs that did work.
    pub duration_ms: Timing,
    /// Spent by runs that did work.
    pub spend: Spend,
    /// Spent by runs that didn't count as work (still going when rolled
    /// up, or held back part way). Not in any view's spend, but a spend
    /// cap counts it.
    pub other_spend: Spend,
    /// The first run that did work.
    pub first_run: Option<DateTime<Utc>>,
    /// In the order [`summarize`] gives them.
    pub steps: Vec<StepRollup>,
}

/// A stored rollup: the table's key and the body.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Rollup {
    pub workflow_id: String,
    pub version: u32,
    /// The UTC day.
    pub day: NaiveDate,
    pub body: DayRollup,
}

impl Rollup {
    pub fn key(&self) -> RollupKey {
        RollupKey {
            workflow_id: self.workflow_id.clone(),
            version: self.version,
            day: self.day,
        }
    }

    /// The body as stored: the same rollup always gives the same text.
    pub fn body_json(&self) -> String {
        serde_json::to_string(&self.body).expect("a rollup serializes")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RollupKey {
    pub workflow_id: String,
    pub version: u32,
    pub day: NaiveDate,
}

/// Rolls up one workflow's runs that started on UTC `day`, one rollup per
/// version that ran. `runs` may hold any runs; the others are ignored. The
/// result depends only on those runs, not on their order.
pub fn roll_up(workflow_id: &str, runs: &[Run], day: NaiveDate) -> Vec<Rollup> {
    let from = day_start(day);
    let window = Window {
        from,
        to: from + Duration::days(1),
        utc_offset_minutes: 0,
    };
    let mut mine: Vec<Run> = runs
        .iter()
        .filter(|r| {
            r.workflow_id == workflow_id
                && !r.dry_run
                && r.started_at >= window.from
                && r.started_at < window.to
        })
        .cloned()
        .collect();
    // A fixed order, so floating-point sums come out the same everywhere.
    mine.sort_by(|a, b| a.started_at.cmp(&b.started_at).then(a.id.cmp(&b.id)));
    let mut versions: Vec<u32> = mine.iter().map(|r| r.version).collect();
    versions.sort_unstable();
    versions.dedup();
    versions
        .into_iter()
        .map(|version| {
            let runs: Vec<Run> = mine
                .iter()
                .filter(|r| r.version == version)
                .cloned()
                .collect();
            Rollup {
                workflow_id: workflow_id.to_string(),
                version,
                day,
                body: body(&runs, &window),
            }
        })
        .collect()
}

fn body(runs: &[Run], window: &Window) -> DayRollup {
    let s = summarize(runs, window);
    let mut duration = Timing::default();
    let mut other_spend = Spend::default();
    // id -> [ms, queue, work, human]
    let mut times: BTreeMap<&str, [Timing; 4]> = BTreeMap::new();
    for run in runs {
        if !did_work(run) {
            for sr in &run.steps {
                other_spend.add(sr);
            }
            continue;
        }
        if let Some(ms) = duration_ms(run) {
            duration.add(ms);
        }
        for sr in &run.steps {
            // The steps `summarize` times.
            if matches!(
                sr.status,
                StepStatus::Skipped | StepStatus::Dry | StepStatus::Waiting
            ) {
                continue;
            }
            let t = times.entry(sr.id.as_str()).or_default();
            t[0].add(step_ms(sr));
            if let Some(m) = &sr.metrics {
                t[1].add(m.queue_ms);
                t[2].add(m.work_ms);
                t[3].add(m.human_wait_ms);
            }
        }
    }
    let steps = s
        .steps
        .iter()
        .map(|st| {
            let [ms, queue_ms, work_ms, human_wait_ms] =
                times.get(st.id.as_str()).cloned().unwrap_or_default();
            StepRollup {
                id: st.id.clone(),
                label: st.label.clone(),
                runs: st.runs,
                ok: st.ok,
                failed: st.failed,
                blocked: st.blocked,
                throttled: st.throttled,
                skipped: st.skipped,
                ms,
                queue_ms,
                work_ms,
                human_wait_ms,
                spend: st.spend.clone(),
                interventions: st.interventions,
                unplanned_interventions: st.unplanned_interventions,
                fallbacks: st.fallbacks,
            }
        })
        .collect();
    DayRollup {
        first_run: s.versions.first().and_then(|v| v.first_run),
        counts: s.counts,
        in_progress: s.in_progress,
        held_back: s.held_back,
        loop_stopped: s.loop_stopped,
        duration_ms: duration,
        spend: s.spend,
        other_spend,
        steps,
    }
}

// ---------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------

/// What a maintenance pass writes: rows to put (new or changed bodies) and
/// rows to delete (a version that no longer has runs that day).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Plan {
    pub put: Vec<Rollup>,
    pub delete: Vec<RollupKey>,
}

/// What to write so the stored rollups match the runs, under the rules in
/// the module docs. `runs` is every run held (any workflow); `existing` is
/// the stored rows, with their bodies as stored, for at least the days
/// `runs` cover.
pub fn plan(now: DateTime<Utc>, runs: &[Run], existing: &[(RollupKey, String)]) -> Plan {
    let today = now.date_naive();
    let log_start = run_log_start(now).date_naive();
    let mut days: BTreeMap<(&str, NaiveDate), ()> = BTreeMap::new();
    for r in runs.iter().filter(|r| !r.dry_run) {
        let day = r.started_at.date_naive();
        if day < today {
            days.insert((r.workflow_id.as_str(), day), ());
        }
    }
    let mut stored: BTreeMap<(&str, NaiveDate), Vec<(&RollupKey, &str)>> = BTreeMap::new();
    for (k, body) in existing {
        stored
            .entry((k.workflow_id.as_str(), k.day))
            .or_default()
            .push((k, body.as_str()));
    }
    let mut out = Plan::default();
    for (workflow_id, day) in days.into_keys() {
        let have = stored
            .get(&(workflow_id, day))
            .map(Vec::as_slice)
            .unwrap_or_default();
        if day < log_start && !have.is_empty() {
            continue; // frozen
        }
        let fresh = roll_up(workflow_id, runs, day);
        for r in &fresh {
            let same = have
                .iter()
                .any(|(k, body)| k.version == r.version && *body == r.body_json());
            if !same {
                out.put.push(r.clone());
            }
        }
        for (k, _) in have {
            if !fresh.iter().any(|r| r.version == k.version) {
                out.delete.push((*k).clone());
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

/// Adds `b` into `a` field by field, through their JSON, so a count or
/// spend field added later is added up too without touching this.
fn add_fields<T: Serialize + DeserializeOwned>(a: &mut T, b: &T) {
    let (Ok(Value::Object(mut x)), Ok(Value::Object(y))) =
        (serde_json::to_value(&*a), serde_json::to_value(b))
    else {
        return;
    };
    for (k, v) in y {
        let sum = match (x.get(&k), &v) {
            (Some(Value::Number(p)), Value::Number(q)) => match (p.as_u64(), q.as_u64()) {
                (Some(p), Some(q)) => Value::from(p.saturating_add(q)),
                _ => Value::from(p.as_f64().unwrap_or(0.0) + q.as_f64().unwrap_or(0.0)),
            },
            (None, v) => v.clone(),
            (Some(p), _) => p.clone(),
        };
        x.insert(k, sum);
    }
    if let Ok(t) = serde_json::from_value(Value::Object(x)) {
        *a = t;
    }
}

#[derive(Default)]
struct Acc {
    counts: Counts,
    duration: Timing,
    spend: Spend,
    first_run: Option<DateTime<Utc>>,
}

impl Acc {
    fn add(&mut self, b: &DayRollup) {
        add_fields(&mut self.counts, &b.counts);
        self.duration.merge(&b.duration_ms);
        add_fields(&mut self.spend, &b.spend);
        if let Some(t) = b.first_run
            && self.first_run.is_none_or(|f| t < f)
        {
            self.first_run = Some(t);
        }
    }
}

/// [`Stats`] from rollups alone: counts, spend, days, versions and steps
/// (their timing from the merged histograms). Models and failure groups
/// aren't in rollups and are left empty.
pub fn combine(rollups: &[Rollup]) -> Stats {
    let mut sorted: Vec<&Rollup> = rollups.iter().collect();
    // Steps come out as first seen, newest version and day first, like
    // `summarize`.
    sorted.sort_by(|a, b| {
        b.version
            .cmp(&a.version)
            .then(b.day.cmp(&a.day))
            .then(a.workflow_id.cmp(&b.workflow_id))
    });
    let mut stats = Stats::default();
    let mut all = Acc::default();
    let mut versions: BTreeMap<u32, Acc> = BTreeMap::new();
    let mut days: BTreeMap<NaiveDate, Acc> = BTreeMap::new();
    let mut steps: Vec<StepRollup> = Vec::new();
    for r in sorted {
        let b = &r.body;
        stats.in_progress += b.in_progress;
        stats.held_back += b.held_back;
        stats.loop_stopped += b.loop_stopped;
        all.add(b);
        if b.counts.runs > 0 {
            versions.entry(r.version).or_default().add(b);
            days.entry(r.day).or_default().add(b);
        }
        for st in &b.steps {
            let at = match steps.iter().position(|s| s.id == st.id) {
                Some(at) => at,
                None => {
                    steps.push(StepRollup {
                        id: st.id.clone(),
                        label: st.label.clone(),
                        ..Default::default()
                    });
                    steps.len() - 1
                }
            };
            let s = &mut steps[at];
            s.runs += st.runs;
            s.ok += st.ok;
            s.failed += st.failed;
            s.blocked += st.blocked;
            s.throttled += st.throttled;
            s.skipped += st.skipped;
            s.ms.merge(&st.ms);
            s.queue_ms.merge(&st.queue_ms);
            s.work_ms.merge(&st.work_ms);
            s.human_wait_ms.merge(&st.human_wait_ms);
            add_fields(&mut s.spend, &st.spend);
            s.interventions += st.interventions;
            s.unplanned_interventions += st.unplanned_interventions;
            s.fallbacks += st.fallbacks;
        }
    }
    stats.counts = all.counts;
    stats.duration_ms = all.duration.quantiles();
    stats.spend = all.spend;
    stats.steps = steps
        .into_iter()
        .map(|s| StepStats {
            id: s.id,
            label: s.label,
            runs: s.runs,
            ok: s.ok,
            failed: s.failed,
            blocked: s.blocked,
            throttled: s.throttled,
            skipped: s.skipped,
            ms: s.ms.quantiles(),
            queue_ms: s.queue_ms.quantiles(),
            work_ms: s.work_ms.quantiles(),
            human_wait_ms: s.human_wait_ms.quantiles(),
            spend: s.spend,
            interventions: s.interventions,
            unplanned_interventions: s.unplanned_interventions,
            fallbacks: s.fallbacks,
        })
        .collect();
    stats.versions = versions
        .into_iter()
        .rev()
        .map(|(version, a)| VersionStats {
            version,
            counts: a.counts,
            duration_ms: a.duration.quantiles(),
            spend: a.spend,
            first_run: a.first_run,
        })
        .collect();
    stats.days = days
        .into_iter()
        .map(|(day, a)| DayStats {
            day,
            counts: a.counts,
            duration_ms: a.duration.quantiles(),
            spend: a.spend,
        })
        .collect();
    stats
}

/// Rolls up runs that started in `[from, to)` (by UTC day; `from` is a
/// UTC midnight), every workflow among them.
fn roll_up_span(runs: &[Run], from: DateTime<Utc>, to: DateTime<Utc>) -> Vec<Rollup> {
    let mut keys: BTreeMap<(&str, NaiveDate), ()> = BTreeMap::new();
    for r in runs
        .iter()
        .filter(|r| !r.dry_run && r.started_at >= from && r.started_at < to)
    {
        keys.insert((r.workflow_id.as_str(), r.started_at.date_naive()), ());
    }
    keys.into_keys()
        .flat_map(|(w, day)| roll_up(w, runs, day))
        .collect()
}

/// Adds up a window that may reach back past the run log: days before
/// `split` (a UTC midnight, normally [`run_log_start`]) from `rollups`,
/// days from it on from `runs`. `window` must break at UTC midnight.
/// Counts, spend and days are exact; step and run timing comes from the
/// merged histograms. Models and failure groups cover only the runs part.
/// A window wholly inside the run log is plain [`summarize`].
pub fn summarize_span(
    runs: &[Run],
    rollups: &[Rollup],
    window: &Window,
    split: DateTime<Utc>,
) -> Stats {
    if window.from >= split {
        return summarize(runs, window);
    }
    let old_to = split.min(window.to);
    let mut parts: Vec<Rollup> = rollups
        .iter()
        .filter(|r| {
            let at = day_start(r.day);
            at >= window.from && at < old_to
        })
        .cloned()
        .collect();
    if window.to > split {
        parts.extend(roll_up_span(runs, split, window.to));
    }
    let mut stats = combine(&parts);
    if window.to > split {
        let recent = summarize(
            runs,
            &Window {
                from: split,
                to: window.to,
                utc_offset_minutes: 0,
            },
        );
        stats.models = recent.models;
        stats.failures = recent.failures;
    }
    stats
}

/// What a workflow spent in `now`'s UTC calendar month: every run that
/// isn't a dry run, whatever its outcome or whether it is still going.
/// The run log always reaches back to the 1st (it keeps 30 whole days
/// before today), so this reads runs; days before the log would come from
/// rollups if that ever changed, never both.
pub fn month_spend(runs: &[Run], rollups: &[Rollup], now: DateTime<Utc>) -> Spend {
    let month = month_start(now);
    let split = run_log_start(now);
    let mut spend = Spend::default();
    let mut old: Vec<&Rollup> = rollups
        .iter()
        .filter(|r| r.day >= month && day_start(r.day) < split)
        .collect();
    old.sort_by_key(|r| r.key());
    for r in old {
        add_fields(&mut spend, &r.body.spend);
        add_fields(&mut spend, &r.body.other_spend);
    }
    let from = split.max(day_start(month));
    for run in runs.iter().filter(|r| !r.dry_run && r.started_at >= from) {
        for sr in &run.steps {
            spend.add(sr);
        }
    }
    spend
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use serde_json::json;

    use super::*;
    use crate::workflow::{
        Billing, Intervention, InterventionKind, RunStatus, StepMetrics, StepRun,
    };

    fn at(day: u32, hour: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, day, hour, 0, 0).unwrap()
    }

    fn d(month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, month, day).unwrap()
    }

    fn agent(status: StepStatus, billing: Billing, cost: f64, work_ms: u64) -> StepRun {
        StepRun {
            id: "fix".into(),
            label: "Fix".into(),
            status,
            input: json!(null),
            output: json!(null),
            error: (status == StepStatus::Failed).then(|| "boom".into()),
            ms: 5,
            metrics: Some(StepMetrics {
                queue_ms: 2_000,
                work_ms,
                billing,
                cost_usd: if billing == Billing::Api { cost } else { 0.0 },
                plan_share: if billing == Billing::Subscription {
                    0.1
                } else {
                    0.0
                },
                input_tokens: 1_000,
                output_tokens: 100,
                harness: "claude_code".into(),
                model: "sonnet".into(),
                ..Default::default()
            }),
            interventions: vec![],
        }
    }

    fn run(id: &str, wf: &str, version: u32, status: RunStatus, start: DateTime<Utc>) -> Run {
        let step = match status {
            RunStatus::Failed => agent(StepStatus::Failed, Billing::Api, 0.25, 30_000),
            RunStatus::Throttled => agent(StepStatus::Throttled, Billing::Api, 0.5, 1_000),
            _ => agent(
                StepStatus::Ok,
                Billing::Api,
                0.125,
                60_000 + id.len() as u64,
            ),
        };
        Run {
            id: id.into(),
            workflow_id: wf.into(),
            workflow_name: wf.into(),
            version,
            status,
            dry_run: false,
            trigger: "t".into(),
            trigger_key: None,
            item_id: None,
            chain: vec![],
            steps: vec![step],
            error: None,
            started_at: start,
            finished_at: Some(start + Duration::seconds(90)),
            runner: String::new(),
            resume: None,
            rerun_of: None,
            started_by: None,
            caused_by: None,
            approved: vec![],
        }
    }

    /// Five weeks of runs of workflow `w`, a few a day, two versions.
    fn history(until: DateTime<Utc>, days: i64) -> Vec<Run> {
        let mut out = Vec::new();
        for n in 0..days * 3 {
            let start = until - Duration::hours(8 * n + 1);
            let status = match n % 7 {
                0 => RunStatus::Failed,
                5 => RunStatus::Throttled,
                _ => RunStatus::Succeeded,
            };
            let mut r = run(&format!("r{n}"), "w", 1 + (n < days) as u32, status, start);
            if n % 11 == 3 {
                r.steps[0].interventions.push(Intervention {
                    kind: InterventionKind::Question,
                    at: start,
                });
                r.steps[0].metrics.as_mut().unwrap().billing = Billing::Subscription;
            }
            out.push(r);
        }
        out
    }

    #[test]
    fn timing_buckets_are_integer_and_quantiles_stay_close() {
        assert_eq!(
            (0..8).map(Timing::bucket).collect::<Vec<_>>(),
            (0..8).collect::<Vec<_>>()
        );
        assert_eq!(
            [8, 9, 10, 15, 16, 19, 20].map(Timing::bucket),
            [8, 8, 9, 11, 12, 12, 13]
        );
        for ms in [
            8u64,
            999,
            1_000,
            60_000,
            3_600_000,
            86_400_000,
            u64::MAX / 2,
        ] {
            let (lo, hi) = Timing::bounds(Timing::bucket(ms));
            assert!(lo <= ms && ms < hi, "{ms} in [{lo}, {hi})");
        }
        assert_eq!(Timing::bounds(Timing::bucket(u64::MAX)).1, u64::MAX);

        let values: Vec<u64> = (1..=500).map(|n| n * n * 37).collect();
        let exact = Quantiles::of(values.clone());
        let t = Timing::of(values.iter().copied()).quantiles();
        assert_eq!(t.n, exact.n);
        for (got, want) in [(t.p50, exact.p50), (t.p95, exact.p95)] {
            let err = (got as f64 - want as f64).abs() / want as f64;
            assert!(err <= 0.125, "{got} vs {want}");
        }
        // Small times are exact, and nothing reads past the largest.
        assert_eq!(Timing::of([3, 3, 5]).quantiles().p50, 3);
        assert_eq!(Timing::of([1_000]).quantile(0.95), 1_000);
        assert_eq!(Timing::default().quantiles(), Quantiles::default());
    }

    #[test]
    fn timing_merges_in_any_order_to_the_same_histogram() {
        let a = Timing::of([10, 2_000, 2_100, 90_000]);
        let b = Timing::of([5, 2_050, 4_000_000]);
        let mut ab = a.clone();
        ab.merge(&b);
        let mut ba = b.clone();
        ba.merge(&a);
        assert_eq!(ab, ba);
        assert_eq!(
            ab,
            Timing::of([10, 2_000, 2_100, 90_000, 5, 2_050, 4_000_000])
        );
        assert_eq!((ab.n, ab.max_ms), (7, 4_000_000));
    }

    #[test]
    fn a_day_rolls_up_by_version_with_its_counts_spend_and_steps() {
        let mut held = run("h", "w", 2, RunStatus::Throttled, at(3, 4));
        held.finished_at = None;
        let mut dry = run("dry", "w", 2, RunStatus::Succeeded, at(3, 5));
        dry.dry_run = true;
        let runs = vec![
            run("a", "w", 2, RunStatus::Succeeded, at(3, 1)),
            run("b", "w", 2, RunStatus::Failed, at(3, 23)),
            run("c", "w", 1, RunStatus::Succeeded, at(3, 2)),
            run("other-day", "w", 2, RunStatus::Succeeded, at(4, 0)),
            run("other-wf", "x", 2, RunStatus::Succeeded, at(3, 3)),
            held,
            dry,
        ];
        let r = roll_up("w", &runs, d(9, 3));
        assert_eq!(r.iter().map(|r| r.version).collect::<Vec<_>>(), [1, 2]);
        let v2 = &r[1].body;
        assert_eq!(
            (v2.counts.runs, v2.counts.succeeded, v2.counts.failed),
            (2, 1, 1)
        );
        assert_eq!((v2.held_back, v2.in_progress), (1, 0));
        assert!((v2.spend.cost_usd - 0.375).abs() < 1e-9);
        // The held-back run's spend isn't in the view's spend, but a cap counts it.
        assert!((v2.other_spend.cost_usd - 0.5).abs() < 1e-9);
        assert_eq!(v2.first_run, Some(at(3, 1)));
        assert_eq!(v2.duration_ms.n, 2);
        let fix = &v2.steps[0];
        assert_eq!((fix.runs, fix.ok, fix.failed), (2, 1, 1));
        assert_eq!(
            (fix.ms.n, fix.work_ms.n, fix.queue_ms.max_ms),
            (2, 2, 2_000)
        );
        // The counts are exactly what `summarize` says for the same runs.
        let s = summarize(
            &runs
                .iter()
                .filter(|r| r.workflow_id == "w" && r.version == 2)
                .cloned()
                .collect::<Vec<_>>(),
            &Window {
                from: day_start(d(9, 3)),
                to: day_start(d(9, 4)),
                utc_offset_minutes: 0,
            },
        );
        assert_eq!(v2.counts, s.counts);
        assert_eq!(v2.spend, s.spend);
        assert!(roll_up("w", &runs, d(9, 9)).is_empty());
    }

    #[test]
    fn the_same_runs_in_any_order_roll_up_to_the_same_text() {
        let now = at(28, 12);
        let runs = history(now, 20);
        let mut shuffled = runs.clone();
        shuffled.reverse();
        shuffled.rotate_left(7);
        for day in [d(9, 20), d(9, 25), d(9, 27)] {
            let a: Vec<String> = roll_up("w", &runs, day)
                .iter()
                .map(Rollup::body_json)
                .collect();
            let b: Vec<String> = roll_up("w", &shuffled, day)
                .iter()
                .map(Rollup::body_json)
                .collect();
            assert!(!a.is_empty());
            assert_eq!(a, b);
        }
    }

    fn stored(rows: &[Rollup]) -> Vec<(RollupKey, String)> {
        rows.iter().map(|r| (r.key(), r.body_json())).collect()
    }

    fn apply(rows: &mut Vec<Rollup>, plan: &Plan) {
        rows.retain(|r| {
            !plan.delete.contains(&r.key()) && !plan.put.iter().any(|p| p.key() == r.key())
        });
        rows.extend(plan.put.iter().cloned());
        rows.sort_by_key(Rollup::key);
    }

    #[test]
    fn two_devices_roll_up_the_same_rows_and_a_second_pass_changes_nothing() {
        let now = at(28, 12);
        let runs = history(now, 40);
        let p = plan(now, &runs, &[]);
        // Every complete day with runs, today left out.
        assert!(p.put.iter().all(|r| r.day < now.date_naive()));
        assert!(p.put.iter().any(|r| r.day == d(9, 27)));
        assert!(p.delete.is_empty());

        // Another device holds the same runs in another order.
        let mut theirs = runs.clone();
        theirs.reverse();
        let q = plan(now, &theirs, &[]);
        let mut a = Vec::new();
        apply(&mut a, &p);
        let mut b = Vec::new();
        apply(&mut b, &q);
        assert_eq!(stored(&a), stored(&b));

        // Again, an hour later: nothing to write.
        let later = now + Duration::hours(1);
        assert_eq!(plan(later, &runs, &stored(&a)), Plan::default());
        // And the other device seeing this device's rows writes nothing either.
        assert_eq!(plan(later, &theirs, &stored(&a)), Plan::default());
    }

    #[test]
    fn a_late_run_is_counted_while_its_day_is_in_the_log_and_not_after() {
        let now = at(28, 12);
        let mut runs = history(now, 10);
        let mut rows = Vec::new();
        apply(&mut rows, &plan(now, &runs, &[]));
        let day = d(9, 25);
        let before = rows
            .iter()
            .filter(|r| r.day == day)
            .map(|r| r.body.counts.runs)
            .sum::<u32>();

        // Another device was offline and its run for the 25th arrives now.
        runs.push(run("late", "w", 1, RunStatus::Succeeded, day_start(day)));
        let p = plan(now, &runs, &stored(&rows));
        assert_eq!(p.put.len(), 1);
        assert_eq!(p.put[0].day, day);
        apply(&mut rows, &p);
        let after = rows
            .iter()
            .filter(|r| r.day == day)
            .map(|r| r.body.counts.runs)
            .sum::<u32>();
        assert_eq!(after, before + 1);

        // Once the day has left the run log, its rollup is frozen: a stray
        // run (the rest are pruned) doesn't rewrite it.
        let much_later = day_start(day) + Duration::days(RUN_LOG_DAYS + 2);
        let stray = vec![run("stray", "w", 1, RunStatus::Failed, day_start(day))];
        assert_eq!(plan(much_later, &stray, &stored(&rows)), Plan::default());
        // But a day this device never rolled up is rolled up once.
        let fresh = plan(much_later, &stray, &[]);
        assert_eq!(fresh.put.len(), 1);
        assert_eq!(fresh.put[0].body.counts.failed, 1);
    }

    #[test]
    fn a_version_that_no_longer_ran_that_day_is_deleted() {
        let now = at(28, 12);
        let runs = vec![
            run("a", "w", 1, RunStatus::Succeeded, at(20, 1)),
            run("b", "w", 2, RunStatus::Succeeded, at(20, 2)),
        ];
        let mut rows = Vec::new();
        apply(&mut rows, &plan(now, &runs, &[]));
        assert_eq!(rows.len(), 2);
        let p = plan(now, &runs[1..], &stored(&rows));
        assert!(p.put.is_empty());
        assert_eq!(p.delete, vec![rows[0].key()]);
    }

    #[test]
    fn a_long_window_reads_old_days_from_rollups_and_recent_ones_from_runs() {
        let now = at(28, 12) + Duration::days(60);
        let all = history(now, 85);
        let split = run_log_start(now);
        // Rollups were written while the old days were in the log; the log
        // now only holds runs from `split` on.
        let mut rows = Vec::new();
        apply(&mut rows, &plan(now, &all, &[]));
        let log: Vec<Run> = all
            .iter()
            .filter(|r| r.started_at >= split)
            .cloned()
            .collect();
        let window = Window {
            from: day_start(now.date_naive() - Duration::days(89)),
            to: day_start(now.date_naive() + Duration::days(1)),
            utc_offset_minutes: 0,
        };
        let span = summarize_span(&log, &rows, &window, split);
        let exact = summarize(&all, &window);
        // Counts, spend and days are exact.
        assert_eq!(span.counts, exact.counts);
        assert_eq!(
            (span.in_progress, span.held_back),
            (exact.in_progress, exact.held_back)
        );
        assert!((span.spend.cost_usd - exact.spend.cost_usd).abs() < 1e-6);
        assert_eq!(span.spend.plan_input_tokens, exact.spend.plan_input_tokens);
        assert_eq!(
            span.days
                .iter()
                .map(|d| (d.day, d.counts.clone()))
                .collect::<Vec<_>>(),
            exact
                .days
                .iter()
                .map(|d| (d.day, d.counts.clone()))
                .collect::<Vec<_>>()
        );
        let ver = |s: &Stats| {
            s.versions
                .iter()
                .map(|v| (v.version, v.counts.runs, v.first_run))
                .collect::<Vec<_>>()
        };
        assert_eq!(ver(&span), ver(&exact));
        assert_eq!(span.steps[0].runs, exact.steps[0].runs);
        // Timing is close.
        let (got, want) = (span.steps[0].work_ms.p50, exact.steps[0].work_ms.p50);
        assert!((got as f64 - want as f64).abs() / want as f64 <= 0.125);
        // Failures only from the run log.
        let recent_failures: u32 =
            log.iter().filter(|r| r.status == RunStatus::Failed).count() as u32;
        assert_eq!(
            span.failures.iter().map(|f| f.count).sum::<u32>(),
            recent_failures
        );
        assert!(recent_failures < exact.failures[0].count);

        // A window wholly in the run log is plain `summarize`.
        let short = Window {
            from: split,
            ..window
        };
        assert_eq!(
            summarize_span(&log, &rows, &short, split),
            summarize(&log, &short)
        );
        // A window wholly before it has no failures or models.
        let old = Window {
            from: window.from,
            to: split,
            utc_offset_minutes: 0,
        };
        let s = summarize_span(&log, &rows, &old, split);
        assert!(s.counts.runs > 0 && s.failures.is_empty() && s.models.is_empty());
    }

    #[test]
    fn month_spend_counts_every_run_this_utc_month_once() {
        // The run log always reaches back to the 1st, even late in a long
        // month, so a month is read from runs and no day is counted twice.
        for n in 0..400 {
            let t = Utc.with_ymd_and_hms(2026, 1, 1, 23, 0, 0).unwrap() + Duration::days(n);
            assert!(run_log_start(t) <= day_start(month_start(t)), "{t}");
        }
        let now = Utc.with_ymd_and_hms(2026, 10, 31, 20, 0, 0).unwrap();
        let all = history(now, 45);
        let mut rows = Vec::new();
        apply(&mut rows, &plan(now, &all, &[]));
        let split = run_log_start(now);
        assert!(rows.iter().any(|r| r.day >= month_start(now)));
        let log: Vec<Run> = all
            .iter()
            .filter(|r| r.started_at >= split)
            .cloned()
            .collect();
        let spent = month_spend(&log, &rows, now);
        let want: f64 = all
            .iter()
            .filter(|r| r.started_at >= day_start(d(10, 1)))
            .flat_map(|r| &r.steps)
            .filter_map(|s| s.metrics.as_ref())
            .filter(|m| m.billing == Billing::Api)
            .map(|m| m.cost_usd)
            .sum();
        assert!(
            (spent.cost_usd - want).abs() < 1e-6,
            "{} vs {want}",
            spent.cost_usd
        );
        assert!(spent.plan_input_tokens > 0);
        // A new month starts from nothing.
        let next = Utc.with_ymd_and_hms(2026, 11, 1, 0, 0, 1).unwrap();
        assert_eq!(month_spend(&log, &rows, next), Spend::default());
    }

    #[test]
    fn days_and_retention_are_utc() {
        let now = Utc.with_ymd_and_hms(2026, 11, 12, 23, 30, 0).unwrap();
        assert_eq!(run_log_start(now), day_start(d(10, 13)));
        assert_eq!(
            keep_rollups_from(now),
            NaiveDate::from_ymd_opt(2025, 10, 12).unwrap()
        );
        assert_eq!(month_start(now), d(11, 1));
    }

    #[test]
    fn old_and_partial_rollup_rows_still_parse() {
        let body: DayRollup = serde_json::from_str(r#"{"counts":{"runs":2,"succeeded":2,"unattended":2,"failed":0,"attended":0,"unplanned":0}}"#).unwrap();
        assert_eq!(body.counts.runs, 2);
        assert_eq!(body.duration_ms, Timing::default());
        // A newer app's extra fields are ignored.
        let newer: DayRollup =
            serde_json::from_str(r#"{"steps":[{"id":"a","p99":1}],"retries":3}"#).unwrap();
        assert_eq!(newer.steps[0].id, "a");
    }

    #[test]
    fn fields_add_up_by_name() {
        let mut a = Spend {
            cost_usd: 0.5,
            input_tokens: 3,
            ..Default::default()
        };
        add_fields(
            &mut a,
            &Spend {
                cost_usd: 0.25,
                input_tokens: 4,
                plan_share: 0.5,
                ..Default::default()
            },
        );
        assert_eq!(
            a,
            Spend {
                cost_usd: 0.75,
                input_tokens: 7,
                plan_share: 0.5,
                ..Default::default()
            }
        );
    }
}
