//! Where an item's workflows stand, in one word for its queue row and
//! header: running, waiting for you, waiting on an agent, failed, held
//! back or done.
//!
//! [`item_statuses`] is a pure function over run records, so every place a
//! hub runs shows the same thing. The caller passes the runs, what each
//! waiting run's agent is doing, and `now`.
//!
//! Which runs count:
//! * Dry runs and runs without an item never do.
//! * Runs stopped by a filter or as a loop don't: they did nothing to the
//!   item worth showing.
//! * A run still going (running, or waiting on an agent) counts for up to
//!   [`OPEN_FOR`] after it started.
//! * A finished run counts only if it's the newest counted run of its
//!   workflow on that item, so a later run replaces an earlier one. A run
//!   blocked on a step that needs the person's OK counts for up to
//!   [`OPEN_FOR`]; a failed or held-back run for [`FAILED_FOR`] after it
//!   ended; a run that finished fine for [`DONE_FOR`].
//!
//! When several runs count, the most pressing state shows, in the order of
//! [`ItemWorkflowState`]; between runs in the same state, the newest.

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use super::{Run, RunStatus};

/// How long a run still going, or blocked on the person's OK, shows.
pub const OPEN_FOR: Duration = Duration::days(7);
/// How long a failed or held-back run shows after it ended.
pub const FAILED_FOR: Duration = Duration::hours(24);
/// How long a run that finished fine shows after it ended.
pub const DONE_FOR: Duration = Duration::hours(1);

/// An item's workflow state, most pressing first.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ItemWorkflowState {
    /// A step needs the person's OK, or the agent a run waits on asked
    /// them something.
    WaitingForYou,
    Running,
    /// Paused on an agent step while a runner works its job.
    WaitingOnAgent,
    Failed,
    /// Over a rate limit, or deferred to protect a budget.
    HeldBack,
    /// Finished fine.
    Done,
}

/// What the agent of a waiting run is doing, by run id.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RunAgent {
    pub job: String,
    /// The agent's chat, once a runner started it.
    pub session: Option<String>,
    /// It asked the person something, or waits on their approval.
    pub needs_you: bool,
}

/// One item's workflow state and the run behind it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ItemWorkflowStatus {
    pub item_id: String,
    pub state: ItemWorkflowState,
    pub run_id: String,
    pub workflow_id: String,
    pub workflow_name: String,
    /// When the run ended, or started if it hasn't.
    pub at: DateTime<Utc>,
    /// The agent chat waiting on the person, when that's why it's
    /// [`ItemWorkflowState::WaitingForYou`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// Other runs on the item that also show a state.
    #[serde(default)]
    pub more: usize,
}

/// Runs a person would want to see on an item at all.
fn counted(r: &Run) -> bool {
    !r.dry_run
        && r.item_id.is_some()
        && !matches!(r.status, RunStatus::Stopped | RunStatus::LoopStopped)
}

/// The state one run shows, if any. `newest` says whether it's the newest
/// counted run of its workflow on its item.
pub fn run_state(
    r: &Run,
    agents: &HashMap<String, RunAgent>,
    newest: bool,
    now: DateTime<Utc>,
) -> Option<ItemWorkflowState> {
    if !counted(r) {
        return None;
    }
    let ended = r.finished_at.unwrap_or(r.started_at);
    let open = now - r.started_at < OPEN_FOR;
    let recent = |within: Duration| newest && now - ended < within;
    use ItemWorkflowState as S;
    match r.status {
        RunStatus::Running => open.then_some(S::Running),
        RunStatus::Waiting => open.then(|| {
            if agents.get(&r.id).is_some_and(|a| a.needs_you) {
                S::WaitingForYou
            } else {
                S::WaitingOnAgent
            }
        }),
        RunStatus::Blocked if r.awaiting_approval().is_some() => {
            (newest && open).then_some(S::WaitingForYou)
        }
        // Blocked before approvals existed: it can't carry on, so it reads
        // as a run that didn't finish.
        RunStatus::Blocked | RunStatus::Failed => recent(FAILED_FOR).then_some(S::Failed),
        RunStatus::Throttled => recent(FAILED_FOR).then_some(S::HeldBack),
        RunStatus::Succeeded => recent(DONE_FOR).then_some(S::Done),
        RunStatus::Stopped | RunStatus::LoopStopped => None,
    }
}

/// Each item's workflow state, from its runs (any order). Items with no
/// run worth showing are left out. Sorted by item id.
pub fn item_statuses(
    runs: &[Run],
    agents: &HashMap<String, RunAgent>,
    now: DateTime<Utc>,
) -> Vec<ItemWorkflowStatus> {
    // The newest counted run of each workflow on each item.
    let mut newest: HashMap<(&str, &str), &Run> = HashMap::new();
    for r in runs.iter().filter(|r| counted(r)) {
        let key = (
            r.item_id.as_deref().unwrap_or_default(),
            r.workflow_id.as_str(),
        );
        let e = newest.entry(key).or_insert(r);
        if (r.started_at, &r.id) > (e.started_at, &e.id) {
            *e = r;
        }
    }
    let mut by_item: BTreeMap<&str, Vec<(ItemWorkflowState, &Run)>> = BTreeMap::new();
    for r in runs {
        let Some(item) = r.item_id.as_deref() else {
            continue;
        };
        let is_newest = newest
            .get(&(item, r.workflow_id.as_str()))
            .is_some_and(|n| n.id == r.id);
        if let Some(state) = run_state(r, agents, is_newest, now) {
            by_item.entry(item).or_default().push((state, r));
        }
    }
    by_item
        .into_iter()
        .filter_map(|(item, mut shown)| {
            let at = |r: &Run| r.finished_at.unwrap_or(r.started_at);
            shown.sort_by(|(sa, ra), (sb, rb)| sa.cmp(sb).then(at(rb).cmp(&at(ra))));
            let more = shown.len().saturating_sub(1);
            let (state, r) = shown.into_iter().next()?;
            let session = (state == ItemWorkflowState::WaitingForYou)
                .then(|| agents.get(&r.id).and_then(|a| a.session.clone()))
                .flatten();
            Some(ItemWorkflowStatus {
                item_id: item.to_string(),
                state,
                run_id: r.id.clone(),
                workflow_id: r.workflow_id.clone(),
                workflow_name: r.workflow_name.clone(),
                at: at(r),
                session,
                more,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::Resume;
    use chrono::TimeZone;
    use serde_json::json;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 3, 10, 12, 0, 0).unwrap()
    }

    fn run(id: &str, workflow: &str, item: &str, status: RunStatus, mins_ago: i64) -> Run {
        let at = now() - Duration::minutes(mins_ago);
        let open = matches!(status, RunStatus::Running | RunStatus::Waiting);
        Run {
            id: id.into(),
            workflow_id: workflow.into(),
            workflow_name: format!("Workflow {workflow}"),
            version: 1,
            status,
            dry_run: false,
            trigger: "t".into(),
            trigger_key: None,
            item_id: Some(item.into()),
            chain: vec![],
            steps: vec![],
            error: None,
            started_at: at,
            finished_at: (!open).then_some(at + Duration::seconds(5)),
            runner: String::new(),
            resume: None,
            rerun_of: None,
            started_by: None,
            caused_by: None,
            approved: vec![],
        }
    }

    fn blocked(id: &str, workflow: &str, item: &str, mins_ago: i64) -> Run {
        let mut r = run(id, workflow, item, RunStatus::Blocked, mins_ago);
        r.resume = Some(Resume {
            job: String::new(),
            step: 1,
            ctx: json!({}),
        });
        r
    }

    fn state_of(runs: &[Run], agents: &HashMap<String, RunAgent>) -> Option<ItemWorkflowState> {
        item_statuses(runs, agents, now()).first().map(|s| s.state)
    }

    #[test]
    fn the_most_pressing_run_shows_and_the_rest_are_counted() {
        let runs = vec![
            run("a", "w1", "i", RunStatus::Succeeded, 10),
            run("b", "w2", "i", RunStatus::Running, 1),
            blocked("c", "w3", "i", 30),
        ];
        let out = item_statuses(&runs, &HashMap::new(), now());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].state, ItemWorkflowState::WaitingForYou);
        assert_eq!((out[0].run_id.as_str(), out[0].more), ("c", 2));
        assert_eq!(out[0].workflow_name, "Workflow w3");
    }

    #[test]
    fn a_waiting_run_is_waiting_for_you_when_its_agent_asks() {
        let runs = vec![run("a", "w", "i", RunStatus::Waiting, 5)];
        assert_eq!(
            state_of(&runs, &HashMap::new()),
            Some(ItemWorkflowState::WaitingOnAgent)
        );
        let mut agents = HashMap::new();
        agents.insert(
            "a".to_string(),
            RunAgent {
                job: "j".into(),
                session: Some("s".into()),
                needs_you: true,
            },
        );
        let out = item_statuses(&runs, &agents, now());
        assert_eq!(out[0].state, ItemWorkflowState::WaitingForYou);
        assert_eq!(out[0].session.as_deref(), Some("s"));
    }

    #[test]
    fn running_beats_waiting_on_an_agent_which_beats_failed() {
        let runs = vec![
            run("a", "w1", "i", RunStatus::Failed, 20),
            run("b", "w2", "i", RunStatus::Waiting, 10),
        ];
        assert_eq!(
            state_of(&runs, &HashMap::new()),
            Some(ItemWorkflowState::WaitingOnAgent)
        );
        let mut more = runs.clone();
        more.push(run("c", "w3", "i", RunStatus::Running, 1));
        assert_eq!(
            state_of(&more, &HashMap::new()),
            Some(ItemWorkflowState::Running)
        );
    }

    #[test]
    fn a_later_run_of_the_same_workflow_replaces_an_earlier_one() {
        let runs = vec![
            run("old", "w", "i", RunStatus::Failed, 30),
            run("new", "w", "i", RunStatus::Succeeded, 5),
        ];
        let out = item_statuses(&runs, &HashMap::new(), now());
        assert_eq!(out[0].state, ItemWorkflowState::Done);
        assert_eq!((out[0].run_id.as_str(), out[0].more), ("new", 0));

        // A newer run also replaces a blocked one: approving stale values
        // makes no sense.
        let runs = vec![
            blocked("old", "w", "i", 30),
            run("new", "w", "i", RunStatus::Succeeded, 5),
        ];
        assert_eq!(
            state_of(&runs, &HashMap::new()),
            Some(ItemWorkflowState::Done)
        );
    }

    #[test]
    fn filtered_dry_and_loop_stopped_runs_never_show_or_replace_others() {
        let mut dry = run("dry", "w", "i", RunStatus::Running, 1);
        dry.dry_run = true;
        let runs = vec![
            run("failed", "w", "i", RunStatus::Failed, 30),
            run("filtered", "w", "i", RunStatus::Stopped, 2),
            run("loop", "w", "i", RunStatus::LoopStopped, 1),
            dry,
        ];
        let out = item_statuses(&runs, &HashMap::new(), now());
        assert_eq!(out[0].state, ItemWorkflowState::Failed);
        assert_eq!(out[0].run_id, "failed");
        assert!(
            item_statuses(&runs[1..], &HashMap::new(), now()).is_empty(),
            "nothing worth showing"
        );
    }

    #[test]
    fn finished_states_fade_done_after_an_hour_and_failures_after_a_day() {
        let done = vec![run("a", "w", "i", RunStatus::Succeeded, 59)];
        assert_eq!(
            state_of(&done, &HashMap::new()),
            Some(ItemWorkflowState::Done)
        );
        let done = vec![run("a", "w", "i", RunStatus::Succeeded, 61)];
        assert_eq!(state_of(&done, &HashMap::new()), None);

        let held = vec![run("a", "w", "i", RunStatus::Throttled, 23 * 60)];
        assert_eq!(
            state_of(&held, &HashMap::new()),
            Some(ItemWorkflowState::HeldBack)
        );
        let failed = vec![run("a", "w", "i", RunStatus::Failed, 25 * 60)];
        assert_eq!(state_of(&failed, &HashMap::new()), None);

        let asking = vec![blocked("a", "w", "i", 6 * 24 * 60)];
        assert_eq!(
            state_of(&asking, &HashMap::new()),
            Some(ItemWorkflowState::WaitingForYou)
        );
        let stale = vec![blocked("a", "w", "i", 8 * 24 * 60)];
        assert_eq!(state_of(&stale, &HashMap::new()), None);
    }

    #[test]
    fn a_run_blocked_before_approvals_existed_reads_as_failed() {
        let runs = vec![run("a", "w", "i", RunStatus::Blocked, 5)];
        assert_eq!(
            state_of(&runs, &HashMap::new()),
            Some(ItemWorkflowState::Failed)
        );
    }

    #[test]
    fn each_item_gets_its_own_status() {
        let mut no_item = run("x", "w", "i", RunStatus::Running, 1);
        no_item.item_id = None;
        let runs = vec![
            run("a", "w", "i2", RunStatus::Running, 1),
            run("b", "w", "i1", RunStatus::Throttled, 1),
            no_item,
        ];
        let out = item_statuses(&runs, &HashMap::new(), now());
        let got: Vec<(&str, ItemWorkflowState)> =
            out.iter().map(|s| (s.item_id.as_str(), s.state)).collect();
        assert_eq!(
            got,
            [
                ("i1", ItemWorkflowState::HeldBack),
                ("i2", ItemWorkflowState::Running)
            ]
        );
    }
}
