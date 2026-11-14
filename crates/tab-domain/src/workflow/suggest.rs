//! What could make a workflow cheaper, steadier or less work for the
//! person, read off its [`Stats`]: a cheaper model family for an agent step
//! that never needs help, a stronger one for a step that often does, more
//! time for a step that runs close to its limit, a higher hourly limit for
//! a workflow that keeps hitting it, a different placement for an agent
//! step that mostly waits for a runner, and approvals the person always
//! grants.
//!
//! Deterministic rules, no model call: the same stats give the same
//! suggestions everywhere. Each rule has a minimum sample, so a workflow
//! with thin data gets no suggestions rather than guesses. A suggestion
//! carries its evidence as numbers and, where there's a concrete change,
//! the workflow with that change made, for the person to review and save.
//! A change never turns on `allow_destructive` or `allow_commands`, and
//! never turns a workflow on: those stay the person's to do.

use serde::{Deserialize, Serialize};

use super::stats::{ModelStats, Spend, Stats, StepStats};
use super::{
    ACTION_STEP_SECS, DEFAULT_COMMAND_SECS, GLOBAL_RUNS_PER_HOUR, MAX_COMMAND_SECS, Step, StepKind,
    Trigger, Workflow,
};
use crate::families::{BUILT_IN, ModelFamily};
use crate::runner::{DEFAULT_JOB_MINUTES, Placement};

/// Runs on a family before a step that never needs help is moved to a
/// cheaper one.
pub const CHEAPER_MIN_RUNS: u32 = 20;
/// Succeeded with nobody stepping in, at least this often.
pub const CHEAPER_MIN_UNATTENDED: f64 = 0.95;
/// A cheaper model already seen on the step needs this many runs, and
/// this unattended rate, to be offered instead of a pinned one.
pub const ALTERNATIVE_MIN_RUNS: u32 = 5;
pub const ALTERNATIVE_MIN_UNATTENDED: f64 = 0.8;
/// Runs on a family before a struggling step is moved to a stronger one.
pub const STRONGER_MIN_RUNS: u32 = 10;
/// Fails at least this often, or …
pub const STRONGER_MAX_FAILURES: f64 = 0.25;
/// … finishes on its own less often than this.
pub const STRONGER_MIN_UNATTENDED: f64 = 0.6;
/// Step runs before its slow end is compared with its time limit.
pub const TIMEOUT_MIN_RUNS: u32 = 10;
/// The slow end (p95) at this share of the limit or more is too close.
pub const TIMEOUT_NEAR: f64 = 0.8;
/// The longest agent deadline a suggestion asks for: a week.
pub const MAX_SUGGESTED_DEADLINE_MINUTES: u32 = 7 * 24 * 60;
/// Hours a workflow hit its own runs-per-hour limit in the window.
pub const LIMIT_MIN_HOURS: u32 = 3;
/// Agent step runs before its queue time is judged.
pub const QUEUE_MIN_RUNS: u32 = 10;
/// The middle wait for a runner, at least this long …
pub const QUEUE_MIN_MS: u64 = 5 * 60_000;
/// … and at least this share of the step's middle time.
pub const QUEUE_SHARE: f64 = 0.5;
/// Approvals granted, with none declined, before pointing out the person
/// could allow the step on the workflow.
pub const APPROVALS_MIN: u32 = 10;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SuggestionKind {
    /// An agent step does fine on its own: a cheaper family or model.
    CheaperModel,
    /// An agent step struggles on a cheap family: the next one up.
    StrongerModel,
    /// A step's slow end is close to its time limit.
    MoreTime,
    /// The workflow keeps hitting its own runs-per-hour limit.
    RateLimit,
    /// An agent step mostly waits for a runner.
    Placement,
    /// The person approves a step every time. Information only.
    AlwaysApproved,
}

/// What a number in the evidence measures, so it can be shown right.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Unit {
    Count,
    /// 0.0 to 1.0.
    Rate,
    Ms,
    Usd,
    Tokens,
}

/// One number behind a suggestion: `value` (of `of`, for a count).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Fact {
    pub label: String,
    pub value: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub of: Option<f64>,
    pub unit: Unit,
}

impl Fact {
    fn count(label: &str, value: u32, of: Option<u32>) -> Self {
        Self {
            label: label.into(),
            value: value as f64,
            of: of.map(|o| o as f64),
            unit: Unit::Count,
        }
    }
    fn of(label: &str, value: f64, unit: Unit) -> Self {
        Self {
            label: label.into(),
            value,
            of: None,
            unit,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Suggestion {
    /// Stable for the same kind on the same step.
    pub id: String,
    pub kind: SuggestionKind,
    /// The step it's about; `None` for the whole workflow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_id: Option<String>,
    /// One line: what to try.
    pub title: String,
    /// One line: the evidence in words.
    pub because: String,
    pub facts: Vec<Fact>,
    /// The workflow with the change made, to review and save; `None` when
    /// there's nothing to change for the person (only to know).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change: Option<Workflow>,
}

/// Suggestions for `workflow` from its stats, most pressing first:
/// reliability, then time limits, hourly limit, placement, cost, and
/// approvals. `families` are the families on offer (built-ins included).
pub fn suggest(workflow: &Workflow, stats: &Stats, families: &[ModelFamily]) -> Vec<Suggestion> {
    let step_stats = |step: &Step| stats.steps.iter().find(|s| s.id == step.id);
    let steps = &workflow.steps;
    let mut out = Vec::new();
    for step in steps {
        out.extend(stronger_model(workflow, step, stats, families));
        out.extend(step_stats(step).and_then(|st| more_time(workflow, step, st)));
    }
    out.extend(rate_limit(workflow, stats));
    for step in steps {
        out.extend(step_stats(step).and_then(|st| placement(workflow, step, st)));
    }
    for step in steps {
        out.extend(cheaper_model(workflow, step, stats, families));
    }
    for step in steps {
        out.extend(step_stats(step).and_then(|st| always_approved(workflow, step, st)));
    }
    out
}

fn name(step: &Step) -> String {
    if step.label.trim().is_empty() {
        step.id.clone()
    } else {
        step.label.clone()
    }
}

fn id(kind: SuggestionKind, step: Option<&str>) -> String {
    let kind = serde_json::to_value(kind)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default();
    match step {
        Some(s) => format!("{kind}:{s}"),
        None => kind,
    }
}

/// The workflow with one step changed.
fn with_step(w: &Workflow, step_id: &str, f: impl FnOnce(&mut Step)) -> Workflow {
    let mut out = w.clone();
    if let Some(s) = out.steps.iter_mut().find(|s| s.id == step_id) {
        f(s);
    }
    out
}

fn pct(r: f64) -> String {
    format!("{}%", (r * 100.0).round() as i64)
}

// ---------------------------------------------------------------------------
// Models
// ---------------------------------------------------------------------------

/// An agent step's model choice: a family, or a pinned harness and model.
enum Choice<'a> {
    Family(&'a str),
    Pinned { harness: &'a str, model: &'a str },
}

fn choice(step: &Step) -> Option<Choice<'_>> {
    let StepKind::Agent {
        harness,
        model,
        family,
        ..
    } = &step.kind
    else {
        return None;
    };
    if harness.is_empty() && model.is_empty() {
        (!family.is_empty()).then_some(Choice::Family(family))
    } else {
        Some(Choice::Pinned { harness, model })
    }
}

/// The built-in families on offer, cheapest first.
fn ladder(families: &[ModelFamily]) -> Vec<&'static str> {
    BUILT_IN
        .iter()
        .copied()
        .filter(|b| families.iter().any(|f| f.id == *b))
        .collect()
}

fn family_name<'a>(id: &'a str, families: &'a [ModelFamily]) -> &'a str {
    families
        .iter()
        .find(|f| f.id == id && !f.name.trim().is_empty())
        .map_or(id, |f| f.name.as_str())
}

/// One step's runs on the models a filter picks, added up.
#[derive(Default)]
struct Tally {
    runs: u32,
    failed: u32,
    unattended: u32,
    spend: Spend,
}

impl Tally {
    fn of<'a>(rows: impl Iterator<Item = &'a ModelStats>) -> Self {
        let mut t = Tally::default();
        for m in rows {
            t.runs += m.runs;
            t.failed += m.failed;
            t.unattended += m.unattended;
            t.spend.cost_usd += m.spend.cost_usd;
            t.spend.input_tokens += m.spend.input_tokens;
            t.spend.output_tokens += m.spend.output_tokens;
        }
        t
    }
    fn unattended_rate(&self) -> f64 {
        self.unattended as f64 / self.runs.max(1) as f64
    }
    fn failure_rate(&self) -> f64 {
        self.failed as f64 / self.runs.max(1) as f64
    }
}

/// What a model costs a run, comparable across the models of one step:
/// dollars when any of them bills through an API key, else tokens.
fn per_run(m: &ModelStats, dollars: bool) -> f64 {
    let n = m.runs.max(1) as f64;
    if dollars {
        m.spend.cost_usd / n
    } else {
        (m.spend.input_tokens + m.spend.output_tokens) as f64 / n
    }
}

fn spend_fact(label: &str, m: &ModelStats, dollars: bool) -> Fact {
    if dollars {
        Fact::of(label, per_run(m, true), Unit::Usd)
    } else {
        Fact::of(label, per_run(m, false), Unit::Tokens)
    }
}

fn cheaper_model(
    w: &Workflow,
    step: &Step,
    stats: &Stats,
    families: &[ModelFamily],
) -> Option<Suggestion> {
    let rows: Vec<&ModelStats> = stats
        .models
        .iter()
        .filter(|m| m.step_id == step.id)
        .collect();
    match choice(step)? {
        Choice::Family(family) => {
            let ladder = ladder(families);
            let at = ladder.iter().position(|f| *f == family)?;
            let cheaper = *ladder.get(at.checked_sub(1)?)?;
            let t = Tally::of(rows.iter().copied().filter(|m| m.family == family));
            if t.runs < CHEAPER_MIN_RUNS || t.unattended_rate() < CHEAPER_MIN_UNATTENDED {
                return None;
            }
            let (now, then) = (
                family_name(family, families),
                family_name(cheaper, families),
            );
            Some(Suggestion {
                id: id(SuggestionKind::CheaperModel, Some(&step.id)),
                kind: SuggestionKind::CheaperModel,
                step_id: Some(step.id.clone()),
                title: format!("Try {} on {then}", name(step)),
                because: format!(
                    "It finished on its own {} of {} times on {now}; {then} is cheaper.",
                    t.unattended, t.runs
                ),
                facts: vec![
                    Fact::count(
                        &format!("Finished on its own on {now}"),
                        t.unattended,
                        Some(t.runs),
                    ),
                    Fact::of("Failed", t.failure_rate(), Unit::Rate),
                ],
                change: Some(with_step(w, &step.id, |s| {
                    if let StepKind::Agent { family, .. } = &mut s.kind {
                        *family = cheaper.to_string();
                    }
                })),
            })
        }
        Choice::Pinned { harness, model } => {
            let mine = |m: &ModelStats| {
                (model.is_empty() || m.model == model)
                    && (harness.is_empty() || m.harness == harness)
            };
            let current: Vec<&ModelStats> = rows.iter().copied().filter(|m| mine(m)).collect();
            let others: Vec<&ModelStats> = rows.iter().copied().filter(|m| !mine(m)).collect();
            let t = Tally::of(current.iter().copied());
            if others.is_empty()
                || t.runs < CHEAPER_MIN_RUNS
                || t.unattended_rate() < CHEAPER_MIN_UNATTENDED
            {
                return None;
            }
            let dollars = rows.iter().any(|m| m.spend.cost_usd > 0.0);
            let cost = if dollars {
                t.spend.cost_usd
            } else {
                (t.spend.input_tokens + t.spend.output_tokens) as f64
            } / t.runs as f64;
            // Only when what it runs on now is the costliest seen.
            if others.iter().any(|m| per_run(m, dollars) > cost) {
                return None;
            }
            let alt = others
                .iter()
                .copied()
                .filter(|m| {
                    m.runs >= ALTERNATIVE_MIN_RUNS
                        && m.unattended as f64 / m.runs as f64 >= ALTERNATIVE_MIN_UNATTENDED
                        && per_run(m, dollars) < cost
                })
                .min_by(|a, b| per_run(a, dollars).total_cmp(&per_run(b, dollars)))?;
            let alt_name = if alt.model.is_empty() {
                format!("{}'s default", alt.harness)
            } else {
                alt.model.clone()
            };
            let now_name = if model.is_empty() { harness } else { model };
            let current_row = ModelStats {
                runs: t.runs,
                spend: t.spend.clone(),
                ..Default::default()
            };
            Some(Suggestion {
                id: id(SuggestionKind::CheaperModel, Some(&step.id)),
                kind: SuggestionKind::CheaperModel,
                step_id: Some(step.id.clone()),
                title: format!("Try {} on {alt_name}", name(step)),
                because: format!(
                    "It finished on its own {} of {} times on {now_name}, the costliest model it ran on; {alt_name} finished on its own {} of {} for less.",
                    t.unattended, t.runs, alt.unattended, alt.runs
                ),
                facts: vec![
                    Fact::count(
                        &format!("On its own on {now_name}"),
                        t.unattended,
                        Some(t.runs),
                    ),
                    Fact::count(
                        &format!("On its own on {alt_name}"),
                        alt.unattended,
                        Some(alt.runs),
                    ),
                    spend_fact(&format!("A run on {now_name}"), &current_row, dollars),
                    spend_fact(&format!("A run on {alt_name}"), alt, dollars),
                ],
                change: Some(with_step(w, &step.id, |s| {
                    if let StepKind::Agent {
                        harness,
                        model,
                        family,
                        ..
                    } = &mut s.kind
                    {
                        *harness = alt.harness.clone();
                        *model = alt.model.clone();
                        family.clear();
                    }
                })),
            })
        }
    }
}

fn stronger_model(
    w: &Workflow,
    step: &Step,
    stats: &Stats,
    families: &[ModelFamily],
) -> Option<Suggestion> {
    let Choice::Family(family) = choice(step)? else {
        return None;
    };
    let ladder = ladder(families);
    let at = ladder.iter().position(|f| *f == family)?;
    let stronger = *ladder.get(at + 1)?;
    let t = Tally::of(
        stats
            .models
            .iter()
            .filter(|m| m.step_id == step.id && m.family == family),
    );
    if t.runs < STRONGER_MIN_RUNS {
        return None;
    }
    let (fails, alone) = (t.failure_rate(), t.unattended_rate());
    if fails < STRONGER_MAX_FAILURES && alone >= STRONGER_MIN_UNATTENDED {
        return None;
    }
    let (now, then) = (
        family_name(family, families),
        family_name(stronger, families),
    );
    let because = if fails >= STRONGER_MAX_FAILURES {
        format!(
            "It failed {} of {} times on {now} ({}).",
            t.failed,
            t.runs,
            pct(fails)
        )
    } else {
        format!(
            "It finished on its own only {} of {} times on {now} ({}); someone had to step in on the rest.",
            t.unattended,
            t.runs,
            pct(alone)
        )
    };
    Some(Suggestion {
        id: id(SuggestionKind::StrongerModel, Some(&step.id)),
        kind: SuggestionKind::StrongerModel,
        step_id: Some(step.id.clone()),
        title: format!("Try {} on {then}", name(step)),
        because,
        facts: vec![
            Fact::count(&format!("Failed on {now}"), t.failed, Some(t.runs)),
            Fact::count(
                &format!("Finished on its own on {now}"),
                t.unattended,
                Some(t.runs),
            ),
        ],
        change: Some(with_step(w, &step.id, |s| {
            if let StepKind::Agent { family, .. } = &mut s.kind {
                *family = stronger.to_string();
            }
        })),
    })
}

// ---------------------------------------------------------------------------
// Time limits
// ---------------------------------------------------------------------------

fn round_up(n: u64, to: u64) -> u64 {
    n.div_ceil(to) * to
}

fn more_time(w: &Workflow, step: &Step, st: &StepStats) -> Option<Suggestion> {
    if st.ms.n < TIMEOUT_MIN_RUNS {
        return None;
    }
    let p95 = st.ms.p95;
    // The limit in ms, and what to raise it to (in the step's own unit)
    // when it can go higher.
    let (limit_ms, raise): (u64, Option<u32>) = match &step.kind {
        StepKind::Agent {
            deadline_minutes, ..
        } => {
            let now = if *deadline_minutes == 0 {
                DEFAULT_JOB_MINUTES
            } else {
                *deadline_minutes
            };
            let want = (round_up(p95 * 3 / 2, 5 * 60_000) / 60_000) as u32;
            let to = want.min(MAX_SUGGESTED_DEADLINE_MINUTES.max(now));
            (now as u64 * 60_000, (to > now).then_some(to))
        }
        StepKind::Command { timeout_secs, .. } => {
            let now = if *timeout_secs == 0 {
                DEFAULT_COMMAND_SECS
            } else {
                *timeout_secs
            };
            let want = (round_up(p95 * 3 / 2, 30_000) / 1000) as u32;
            let to = want.min(MAX_COMMAND_SECS.max(now));
            (now as u64 * 1000, (to > now).then_some(to))
        }
        StepKind::Action { .. } => (ACTION_STEP_SECS as u64 * 1000, None),
        _ => return None,
    };
    if (p95 as f64) < limit_ms as f64 * TIMEOUT_NEAR {
        return None;
    }
    let limit_words = human_ms(limit_ms);
    let title = match (&step.kind, raise) {
        (StepKind::Agent { .. }, Some(m)) => {
            format!(
                "Give {} {} before it's given up",
                name(step),
                human_ms(m as u64 * 60_000)
            )
        }
        (StepKind::Command { .. }, Some(s)) => {
            format!("Give {} {}", name(step), human_ms(s as u64 * 1000))
        }
        (StepKind::Action { .. }, _) => format!(
            "{} runs close to the {limit_words} action limit",
            name(step)
        ),
        _ => format!("{} runs close to its {limit_words} limit", name(step)),
    };
    let because = format!(
        "1 in 20 runs takes {} or more, against a limit of {limit_words}.",
        human_ms(p95)
    );
    let change = raise.map(|to| {
        with_step(w, &step.id, |s| match &mut s.kind {
            StepKind::Agent {
                deadline_minutes, ..
            } => *deadline_minutes = to,
            StepKind::Command { timeout_secs, .. } => *timeout_secs = to,
            _ => {}
        })
    });
    Some(Suggestion {
        id: id(SuggestionKind::MoreTime, Some(&step.id)),
        kind: SuggestionKind::MoreTime,
        step_id: Some(step.id.clone()),
        title,
        because,
        facts: vec![
            Fact::of("Slow end (p95)", p95 as f64, Unit::Ms),
            Fact::of("Limit", limit_ms as f64, Unit::Ms),
            Fact::count("Runs", st.ms.n, None),
            Fact::count("Failed", st.failed, Some(st.runs)),
        ],
        change,
    })
}

fn human_ms(ms: u64) -> String {
    let s = ms / 1000;
    match s {
        0..60 => format!("{s} s"),
        60..3600 => {
            let (m, r) = (s / 60, s % 60);
            if r == 0 {
                format!("{m} min")
            } else {
                format!("{m} min {r} s")
            }
        }
        _ => {
            let (h, m) = (s / 3600, (s % 3600) / 60);
            if m == 0 {
                format!("{h} h")
            } else {
                format!("{h} h {m} min")
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The hourly limit
// ---------------------------------------------------------------------------

fn rate_limit(w: &Workflow, stats: &Stats) -> Option<Suggestion> {
    if stats.held_by_limit < LIMIT_MIN_HOURS {
        return None;
    }
    let now = w.limits.max_runs_per_hour;
    let to = (now.saturating_mul(2)).min(GLOBAL_RUNS_PER_HOUR as u32);
    let per_item = matches!(w.trigger, Trigger::Item { .. }) && w.limits.cooldown_minutes == 0;
    let because = format!(
        "It hit its limit of {now} runs an hour in {} separate hours, and triggers past it were skipped.{}",
        stats.held_by_limit,
        if per_item {
            " If the same items keep starting it, a cool-down per item may suit better."
        } else {
            ""
        }
    );
    Some(Suggestion {
        id: id(SuggestionKind::RateLimit, None),
        kind: SuggestionKind::RateLimit,
        step_id: None,
        title: if to > now {
            format!("Raise its limit to {to} runs an hour")
        } else {
            format!("It keeps hitting its limit of {now} runs an hour")
        },
        because,
        facts: vec![
            Fact::count("Hours it hit its limit", stats.held_by_limit, None),
            Fact::count("Runs an hour allowed", now, None),
            Fact::count("Runs that did work", stats.counts.runs, None),
        ],
        change: (to > now).then(|| {
            let mut out = w.clone();
            out.limits.max_runs_per_hour = to;
            out
        }),
    })
}

// ---------------------------------------------------------------------------
// Placement
// ---------------------------------------------------------------------------

fn placement(w: &Workflow, step: &Step, st: &StepStats) -> Option<Suggestion> {
    let StepKind::Agent { placement, .. } = &step.kind else {
        return None;
    };
    if st.queue_ms.n < QUEUE_MIN_RUNS {
        return None;
    }
    let (queue, work) = (st.queue_ms.p50, st.work_ms.p50);
    if queue < QUEUE_MIN_MS || (queue as f64) < (queue + work) as f64 * QUEUE_SHARE {
        return None;
    }
    let (to, title) = match placement {
        Placement::Computers => (
            Some(Placement::PreferComputer { minutes: 10 }),
            format!(
                "Let tab cloud take {} when no computer does in 10 min",
                name(step)
            ),
        ),
        Placement::PreferComputer { minutes } if *minutes > 5 => {
            let m = (*minutes / 2).max(5);
            (
                Some(Placement::PreferComputer { minutes: m }),
                format!(
                    "Hand {} to tab cloud after {m} min, not {minutes}",
                    name(step)
                ),
            )
        }
        Placement::Cloud => (
            Some(Placement::Anywhere),
            format!("Let any of your runners take {}", name(step)),
        ),
        _ => (
            None,
            format!("{} waits a long time for a runner", name(step)),
        ),
    };
    Some(Suggestion {
        id: id(SuggestionKind::Placement, Some(&step.id)),
        kind: SuggestionKind::Placement,
        step_id: Some(step.id.clone()),
        title,
        because: format!(
            "Half its runs wait {} or more for a runner, longer than the agent then works ({}).",
            human_ms(queue),
            human_ms(work)
        ),
        facts: vec![
            Fact::of("Middle wait for a runner", queue as f64, Unit::Ms),
            Fact::of("Middle work", work as f64, Unit::Ms),
            Fact::count("Runs", st.queue_ms.n, None),
        ],
        change: to.map(|p| {
            with_step(w, &step.id, |s| {
                if let StepKind::Agent { placement, .. } = &mut s.kind {
                    *placement = p;
                }
            })
        }),
    })
}

// ---------------------------------------------------------------------------
// Approvals
// ---------------------------------------------------------------------------

fn always_approved(w: &Workflow, step: &Step, st: &StepStats) -> Option<Suggestion> {
    if st.approved < APPROVALS_MIN || st.declined > 0 {
        return None;
    }
    let (what, already) = match &step.kind {
        StepKind::Command { .. } => ("commands", w.allow_commands),
        StepKind::Action { .. } => ("destructive steps", w.allow_destructive),
        _ => return None,
    };
    if already {
        return None;
    }
    Some(Suggestion {
        id: id(SuggestionKind::AlwaysApproved, Some(&step.id)),
        kind: SuggestionKind::AlwaysApproved,
        step_id: Some(step.id.clone()),
        title: format!("You've OK'd {} every time", name(step)),
        because: format!(
            "You approved it all {} times it asked and never declined. If you trust it, you can allow {what} on this workflow so it stops asking; only you can.",
            st.approved
        ),
        facts: vec![
            Fact::count("Approved", st.approved, None),
            Fact::count("Declined", st.declined, None),
        ],
        // Consent stays with the person: never a change.
        change: None,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::Map;

    use super::*;
    use crate::families::Candidate;
    use crate::workflow::stats::Quantiles;
    use crate::workflow::{Limits, Trigger};

    fn fams() -> Vec<ModelFamily> {
        BUILT_IN
            .iter()
            .map(|id| ModelFamily {
                id: id.to_string(),
                name: String::new(),
                candidates: vec![Candidate::new("claude_code", "")],
            })
            .collect()
    }

    fn agent(id: &str, family: &str) -> Step {
        Step {
            id: id.into(),
            label: format!("Step {id}"),
            when: vec![],
            continue_on_error: false,
            kind: StepKind::Agent {
                prompt: "Fix it".into(),
                repo: String::new(),
                harness: String::new(),
                model: String::new(),
                family: family.into(),
                placement: Placement::Computers,
                deadline_minutes: 0,
            },
        }
    }

    fn action(id: &str) -> Step {
        Step {
            id: id.into(),
            label: String::new(),
            when: vec![],
            continue_on_error: false,
            kind: StepKind::Action {
                connector: "github".into(),
                action: "pr.merge".into(),
                input: Map::new(),
            },
        }
    }

    fn command(id: &str, timeout_secs: u32) -> Step {
        Step {
            id: id.into(),
            label: "Tests".into(),
            when: vec![],
            continue_on_error: false,
            kind: StepKind::Command {
                program: "cargo".into(),
                args: vec!["test".into()],
                folder: String::new(),
                network: vec![],
                env: Map::new(),
                image: String::new(),
                timeout_secs,
            },
        }
    }

    fn workflow(steps: Vec<Step>) -> Workflow {
        Workflow {
            id: "w".into(),
            name: "W".into(),
            trigger: Trigger::Manual,
            steps,
            limits: Limits::default(),
            version: 3,
            ..Default::default()
        }
    }

    fn model(
        step: &str,
        family: &str,
        m: &str,
        runs: u32,
        failed: u32,
        unattended: u32,
        usd: f64,
    ) -> ModelStats {
        ModelStats {
            step_id: step.into(),
            harness: "claude_code".into(),
            model: m.into(),
            family: family.into(),
            runs,
            ok: runs - failed,
            failed,
            unattended,
            spend: Spend {
                cost_usd: usd,
                input_tokens: 1_000 * runs as u64,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn step_stats(id: &str, runs: u32) -> StepStats {
        StepStats {
            id: id.into(),
            runs,
            ok: runs,
            ..Default::default()
        }
    }

    fn q(n: u32, p50: u64, p95: u64) -> Quantiles {
        Quantiles { n, p50, p95 }
    }

    fn stats(steps: Vec<StepStats>, models: Vec<ModelStats>) -> Stats {
        Stats {
            steps,
            models,
            ..Default::default()
        }
    }

    fn only(s: Vec<Suggestion>, kind: SuggestionKind) -> Vec<Suggestion> {
        s.into_iter().filter(|x| x.kind == kind).collect()
    }

    fn family_of(w: &Workflow, step: &str) -> String {
        match &w.steps.iter().find(|s| s.id == step).unwrap().kind {
            StepKind::Agent { family, .. } => family.clone(),
            _ => unreachable!(),
        }
    }

    #[test]
    fn a_deep_step_that_never_needs_help_tries_balanced() {
        let w = workflow(vec![agent("fix", "deep")]);
        let s = stats(
            vec![step_stats("fix", 20)],
            vec![model("fix", "deep", "opus", 20, 0, 19, 12.0)],
        );
        let out = only(suggest(&w, &s, &fams()), SuggestionKind::CheaperModel);
        assert_eq!(out.len(), 1);
        let x = &out[0];
        assert_eq!(x.step_id.as_deref(), Some("fix"));
        assert_eq!(x.title, "Try Step fix on balanced");
        assert!(x.because.contains("19 of 20"), "{}", x.because);
        assert_eq!((x.facts[0].value, x.facts[0].of), (19.0, Some(20.0)));
        let changed = x.change.as_ref().unwrap();
        assert_eq!(family_of(changed, "fix"), "balanced");
        assert_eq!((changed.id.as_str(), changed.version), ("w", 3));
    }

    #[test]
    fn a_cheaper_family_needs_twenty_runs_and_ninety_five_percent() {
        let w = workflow(vec![agent("fix", "deep")]);
        let thin = stats(vec![], vec![model("fix", "deep", "opus", 19, 0, 19, 1.0)]);
        assert!(suggest(&w, &thin, &fams()).is_empty());
        let shaky = stats(vec![], vec![model("fix", "deep", "opus", 40, 1, 37, 1.0)]);
        assert!(suggest(&w, &shaky, &fams()).is_empty());
        // Nothing cheaper than fast.
        let w = workflow(vec![agent("fix", "fast")]);
        let fine = stats(vec![], vec![model("fix", "fast", "haiku", 40, 0, 40, 1.0)]);
        assert!(suggest(&w, &fine, &fams()).is_empty());
    }

    #[test]
    fn a_pinned_model_that_is_the_costliest_seen_tries_a_cheaper_one_that_did_well() {
        let mut w = workflow(vec![agent("fix", "")]);
        if let StepKind::Agent {
            harness, model: m, ..
        } = &mut w.steps[0].kind
        {
            *harness = "claude_code".into();
            *m = "opus".into();
        }
        let s = stats(
            vec![],
            vec![
                model("fix", "", "opus", 20, 0, 20, 10.0),
                model("fix", "", "sonnet", 6, 0, 5, 1.2),
            ],
        );
        let out = only(suggest(&w, &s, &fams()), SuggestionKind::CheaperModel);
        assert_eq!(out.len(), 1);
        match &out[0].change.as_ref().unwrap().steps[0].kind {
            StepKind::Agent { model, harness, .. } => {
                assert_eq!(
                    (harness.as_str(), model.as_str()),
                    ("claude_code", "sonnet")
                )
            }
            _ => unreachable!(),
        }
        // Too little on the other model to go on.
        let s = stats(
            vec![],
            vec![
                model("fix", "", "opus", 20, 0, 20, 10.0),
                model("fix", "", "sonnet", 4, 0, 4, 0.8),
            ],
        );
        assert!(suggest(&w, &s, &fams()).is_empty());
    }

    #[test]
    fn a_struggling_step_on_a_cheap_family_tries_the_next_one_up() {
        let w = workflow(vec![agent("fix", "fast")]);
        let failing = stats(vec![], vec![model("fix", "fast", "haiku", 10, 3, 6, 0.5)]);
        let out = only(
            suggest(&w, &failing, &fams()),
            SuggestionKind::StrongerModel,
        );
        assert_eq!(out.len(), 1);
        assert!(
            out[0].because.contains("failed 3 of 10"),
            "{}",
            out[0].because
        );
        assert_eq!(
            family_of(out[0].change.as_ref().unwrap(), "fix"),
            "balanced"
        );

        let needs_help = stats(vec![], vec![model("fix", "fast", "haiku", 12, 0, 6, 0.5)]);
        let out = only(
            suggest(&w, &needs_help, &fams()),
            SuggestionKind::StrongerModel,
        );
        assert!(
            out[0].because.contains("only 6 of 12"),
            "{}",
            out[0].because
        );

        // Under ten runs says nothing.
        let thin = stats(vec![], vec![model("fix", "fast", "haiku", 9, 5, 2, 0.5)]);
        assert!(suggest(&w, &thin, &fams()).is_empty());
        // Nothing stronger than deep.
        let w = workflow(vec![agent("fix", "deep")]);
        let deep = stats(vec![], vec![model("fix", "deep", "opus", 20, 10, 5, 5.0)]);
        assert!(only(suggest(&w, &deep, &fams()), SuggestionKind::StrongerModel).is_empty());
    }

    #[test]
    fn a_step_near_its_time_limit_gets_more_time() {
        let w = workflow(vec![command("tests", 600)]);
        let mut st = step_stats("tests", 12);
        st.ms = q(12, 300_000, 520_000);
        let out = suggest(&w, &stats(vec![st.clone()], vec![]), &fams());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, SuggestionKind::MoreTime);
        assert_eq!(out[0].title, "Give Tests 13 min");
        match &out[0].change.as_ref().unwrap().steps[0].kind {
            StepKind::Command { timeout_secs, .. } => assert_eq!(*timeout_secs, 780),
            _ => unreachable!(),
        }

        // Comfortably inside it, or too few runs: nothing.
        let mut fine = st.clone();
        fine.ms = q(12, 100_000, 400_000);
        assert!(suggest(&w, &stats(vec![fine], vec![]), &fams()).is_empty());
        let mut thin = st.clone();
        thin.ms = q(9, 300_000, 590_000);
        assert!(suggest(&w, &stats(vec![thin], vec![]), &fams()).is_empty());

        // At the longest a command may take: flagged, nothing to change.
        let w = workflow(vec![command("tests", MAX_COMMAND_SECS)]);
        let mut at_max = st;
        at_max.ms = q(12, 3_000_000, 3_500_000);
        let out = suggest(&w, &stats(vec![at_max], vec![]), &fams());
        assert!(out[0].change.is_none());

        // Agent steps by their deadline; actions only get a flag.
        let mut w = workflow(vec![agent("fix", ""), action("merge")]);
        if let StepKind::Agent {
            deadline_minutes, ..
        } = &mut w.steps[0].kind
        {
            *deadline_minutes = 45;
        }
        let mut fix = step_stats("fix", 10);
        fix.ms = q(10, 1_200_000, 2_400_000);
        let mut merge = step_stats("merge", 10);
        merge.ms = q(10, 20_000, 55_000);
        let out = suggest(&w, &stats(vec![fix, merge], vec![]), &fams());
        assert_eq!(out.len(), 2);
        match &out[0].change.as_ref().unwrap().steps[0].kind {
            StepKind::Agent {
                deadline_minutes, ..
            } => assert_eq!(*deadline_minutes, 60),
            _ => unreachable!(),
        }
        assert!(
            out[1].title.contains("1 min action limit"),
            "{}",
            out[1].title
        );
        assert!(out[1].change.is_none());
    }

    #[test]
    fn a_workflow_that_keeps_hitting_its_hourly_limit_may_raise_it() {
        let mut w = workflow(vec![]);
        w.trigger = Trigger::default();
        let mut s = stats(vec![], vec![]);
        s.held_by_limit = 3;
        let out = suggest(&w, &s, &fams());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "Raise its limit to 60 runs an hour");
        assert!(out[0].because.contains("cool-down"));
        assert_eq!(out[0].change.as_ref().unwrap().limits.max_runs_per_hour, 60);

        s.held_by_limit = 2;
        assert!(suggest(&w, &s, &fams()).is_empty());

        // Never past the cap across every workflow.
        s.held_by_limit = 5;
        w.limits.max_runs_per_hour = GLOBAL_RUNS_PER_HOUR as u32;
        assert!(suggest(&w, &s, &fams())[0].change.is_none());
    }

    #[test]
    fn an_agent_step_that_mostly_waits_for_a_runner_gets_a_wider_placement() {
        let w = workflow(vec![agent("fix", "")]);
        let mut st = step_stats("fix", 10);
        st.queue_ms = q(10, 20 * 60_000, 60 * 60_000);
        st.work_ms = q(10, 8 * 60_000, 20 * 60_000);
        let out = suggest(&w, &stats(vec![st.clone()], vec![]), &fams());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, SuggestionKind::Placement);
        match &out[0].change.as_ref().unwrap().steps[0].kind {
            StepKind::Agent { placement, .. } => {
                assert_eq!(*placement, Placement::PreferComputer { minutes: 10 })
            }
            _ => unreachable!(),
        }

        // A short wait, a wait shorter than the work, or too few runs: nothing.
        let mut short = st.clone();
        short.queue_ms = q(10, 4 * 60_000, 10 * 60_000);
        short.work_ms = q(10, 60_000, 60_000);
        let mut small_share = st.clone();
        small_share.work_ms = q(10, 30 * 60_000, 60 * 60_000);
        let mut thin = st;
        thin.queue_ms.n = 9;
        for x in [short, small_share, thin] {
            assert!(suggest(&w, &stats(vec![x], vec![]), &fams()).is_empty());
        }
    }

    #[test]
    fn an_approval_granted_every_time_is_pointed_out_but_never_changed() {
        let w = workflow(vec![action("merge"), command("tests", 0)]);
        let mut merge = step_stats("merge", 12);
        merge.approved = 12;
        let mut tests = step_stats("tests", 10);
        tests.approved = 10;
        let out = suggest(
            &w,
            &stats(vec![merge.clone(), tests.clone()], vec![]),
            &fams(),
        );
        assert_eq!(out.len(), 2);
        assert!(
            out.iter()
                .all(|x| x.kind == SuggestionKind::AlwaysApproved && x.change.is_none())
        );
        assert!(out[0].because.contains("destructive steps"));
        assert!(out[1].because.contains("commands"));

        // One no, too few, or already allowed: nothing.
        let mut once_declined = merge.clone();
        once_declined.declined = 1;
        let mut few = merge.clone();
        few.approved = 9;
        for x in [once_declined, few] {
            assert!(suggest(&w, &stats(vec![x], vec![]), &fams()).is_empty());
        }
        let mut allowed = w;
        allowed.allow_destructive = true;
        assert!(suggest(&allowed, &stats(vec![merge], vec![]), &fams()).is_empty());
    }

    #[test]
    fn no_change_ever_turns_on_destructive_steps_commands_or_the_workflow() {
        let mut w = workflow(vec![
            agent("fix", "deep"),
            command("tests", 600),
            action("merge"),
        ]);
        w.trigger = Trigger::default();
        let mut fix = step_stats("fix", 30);
        fix.queue_ms = q(30, 30 * 60_000, 60 * 60_000);
        fix.work_ms = q(30, 60_000, 60_000);
        fix.ms = q(30, 30 * 60_000, 23 * 3_600_000);
        let mut tests = step_stats("tests", 30);
        tests.ms = q(30, 500_000, 590_000);
        tests.approved = 30;
        let mut merge = step_stats("merge", 30);
        merge.approved = 30;
        let mut s = stats(
            vec![fix, tests, merge],
            vec![model("fix", "deep", "opus", 30, 0, 30, 9.0)],
        );
        s.held_by_limit = 9;
        let out = suggest(&w, &s, &fams());
        assert!(out.len() >= 5, "{out:#?}");
        for x in &out {
            if let Some(c) = &x.change {
                assert!(
                    !c.allow_destructive && !c.allow_commands && !c.enabled,
                    "{}",
                    x.id
                );
                assert_eq!(c.id, w.id);
            }
        }
        // Ids are unique, for the UI's keys.
        let mut ids: Vec<&str> = out.iter().map(|x| x.id.as_str()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), out.len());
    }

    #[test]
    fn no_runs_suggest_nothing() {
        let w = workflow(vec![
            agent("fix", "deep"),
            command("tests", 0),
            action("merge"),
        ]);
        assert!(suggest(&w, &Stats::default(), &fams()).is_empty());
    }

    #[test]
    fn suggestions_serialize_compactly() {
        let w = workflow(vec![action("merge")]);
        let mut merge = step_stats("merge", 10);
        merge.approved = 10;
        let v = serde_json::to_value(suggest(&w, &stats(vec![merge], vec![]), &fams())).unwrap();
        assert_eq!(v[0]["kind"], "always_approved");
        assert_eq!(v[0]["id"], "always_approved:merge");
        assert!(v[0].get("change").is_none());
        assert_eq!(v[0]["facts"][0]["unit"], "count");
        let back: Vec<Suggestion> = serde_json::from_value(v.clone()).unwrap();
        assert_eq!(serde_json::to_value(back).unwrap(), v);
    }
}
