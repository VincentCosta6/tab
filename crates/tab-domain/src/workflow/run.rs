//! A workflow run as it's stored and shown: its status and each step's
//! inputs and outputs. The runtime that produces runs lives with the desktop
//! engine (`tab_core::workflow::runtime`); these are the shapes every place a
//! hub runs agrees on.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Running,
    Succeeded,
    /// A filter step ended it early. Not a failure.
    Stopped,
    Failed,
    /// A destructive step needs the person's OK.
    Blocked,
    /// Over a rate limit, or the gateway deferred an action to protect a budget.
    Throttled,
    /// Started inside its own chain, or the chain got too long.
    LoopStopped,
    /// Paused at an agent step until a runner finishes its job.
    Waiting,
}

impl RunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            RunStatus::Running => "running",
            RunStatus::Succeeded => "succeeded",
            RunStatus::Stopped => "stopped",
            RunStatus::Failed => "failed",
            RunStatus::Blocked => "blocked",
            RunStatus::Throttled => "throttled",
            RunStatus::LoopStopped => "loop_stopped",
            RunStatus::Waiting => "waiting",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "running" => RunStatus::Running,
            "succeeded" => RunStatus::Succeeded,
            "stopped" => RunStatus::Stopped,
            "blocked" => RunStatus::Blocked,
            "throttled" => RunStatus::Throttled,
            "loop_stopped" => RunStatus::LoopStopped,
            "waiting" => RunStatus::Waiting,
            _ => RunStatus::Failed,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Ok,
    /// Its `when` didn't hold.
    Skipped,
    /// A filter that ended the run.
    Stopped,
    Failed,
    Blocked,
    Throttled,
    /// Dry run: a write that would have happened with these inputs.
    Dry,
    /// Handed to a runner as a job; the run carries on when it's done.
    Waiting,
}

/// Where a paused run picks up, with the run's values as they were.
///
/// A run waiting on an agent carries on at the step after the agent step
/// once job `job` finishes. A run blocked on a destructive or command step
/// has no job (`job` is empty): if the person approves that step for this
/// run, it runs that step and carries on.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Resume {
    pub job: String,
    /// Index of the paused step in the workflow version that ran.
    pub step: usize,
    pub ctx: Value,
}

/// A step the person let run once, on one run, after it was blocked.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StepApproval {
    pub step: String,
    /// The inputs they saw. The step runs only if it renders the same.
    #[serde(default)]
    pub input: Value,
    pub at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StepRun {
    pub id: String,
    pub label: String,
    pub status: StepStatus,
    #[serde(default)]
    pub input: Value,
    #[serde(default)]
    pub output: Value,
    pub error: Option<String>,
    pub ms: u64,
    /// What an agent step took and spent. Absent on other steps, and on
    /// runs from before this was recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics: Option<StepMetrics>,
    /// Each time a person had to step in on this step.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub interventions: Vec<Intervention>,
}

/// What an agent step took and spent, filled in when its job finishes.
/// Everything defaults to zero or empty, so a runner that reports less
/// (an older one, or a harness that doesn't say what it cost) still fits.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct StepMetrics {
    /// From the job being created to a runner claiming it.
    pub queue_ms: u64,
    /// From the claim to the result.
    pub work_ms: u64,
    /// Time inside `work_ms` spent waiting on the person.
    pub human_wait_ms: u64,
    /// How the session was paid for, which decides what its cost reads
    /// as: dollars through an API key, usage on a subscription.
    pub billing: Billing,
    /// What it was billed. Zero on a subscription, where nothing is
    /// billed per run.
    pub cost_usd: f64,
    /// On a subscription: how much of the plan's allowance the step used,
    /// 0.0 to 1.0, when the harness reports its plan windows.
    pub plan_share: f64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// The model family the step asked for; empty when it pinned a model
    /// or took the runner's default.
    pub family: String,
    /// The harness and model that ran it.
    pub harness: String,
    pub model: String,
    /// Claims made on the job.
    pub attempts: u32,
    /// Times the runner moved on to the family's next model.
    pub fallbacks: u32,
}

/// How an agent session is paid for.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Billing {
    /// Per token, through an API key: the cost is dollars.
    Api,
    /// A plan with usage windows: the cost is tokens and plan share.
    Subscription,
    /// The harness didn't say (or said something newer than this build).
    #[default]
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum InterventionKind {
    /// An OK the workflow asks for by design (a destructive step).
    Approval,
    /// The agent stopped to ask the person something.
    Question,
    /// The person wrote into the job's chat unasked.
    Message,
    /// The person stopped the job.
    Stopped,
    /// The person ran it again after it failed.
    Rerun,
}

impl InterventionKind {
    /// Planned interventions are part of the workflow's design; the rest
    /// mean it couldn't finish on its own.
    pub fn planned(self) -> bool {
        matches!(self, InterventionKind::Approval)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Intervention {
    pub kind: InterventionKind,
    pub at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Run {
    pub id: String,
    pub workflow_id: String,
    pub workflow_name: String,
    pub version: u32,
    pub status: RunStatus,
    pub dry_run: bool,
    /// What started it, in words.
    pub trigger: String,
    /// Dedup key: the same key runs a workflow once.
    pub trigger_key: Option<String>,
    pub item_id: Option<String>,
    /// Workflows that led to this run, oldest first.
    pub chain: Vec<String>,
    pub steps: Vec<StepRun>,
    pub error: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    /// The runner (device id) that started it, so only that runner closes
    /// it if the app quits mid-run. Empty on runs from before runners.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub runner: String,
    /// Set while the run waits on an agent step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume: Option<Resume>,
    /// The failed run a person ran again to get this one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rerun_of: Option<String>,
    /// The agent chat (session id) that started it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_by: Option<String>,
    /// The run that led to this one, when it's known: the run whose agent
    /// started it, or whose signal it answered. Runs started by an item
    /// change another run made only have `chain`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caused_by: Option<String>,
    /// Steps the person let run on this run only (never the workflow's
    /// `allow_destructive`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub approved: Vec<StepApproval>,
}

impl Run {
    /// Where a blocked run would carry on if the person approved its
    /// blocked step. `None` for every other run, and for runs blocked
    /// before approvals existed, which can't carry on.
    pub fn awaiting_approval(&self) -> Option<&Resume> {
        self.resume
            .as_ref()
            .filter(|r| self.status == RunStatus::Blocked && !self.dry_run && r.job.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_stored_before_approvals_and_agent_starts_still_parses() {
        let old = r#"{"id":"r","workflow_id":"w","workflow_name":"W","version":1,"status":"blocked",
            "dry_run":false,"trigger":"t","trigger_key":null,"item_id":"i","chain":[],"steps":[],
            "error":"Merge is destructive.","started_at":"2026-01-02T03:04:05Z",
            "finished_at":"2026-01-02T03:04:06Z"}"#;
        let run: Run = serde_json::from_str(old).unwrap();
        assert_eq!(run.started_by, None);
        assert_eq!(run.caused_by, None);
        assert!(run.approved.is_empty());
        assert!(
            run.awaiting_approval().is_none(),
            "nothing to carry on from"
        );
        let again = serde_json::to_value(&run).unwrap();
        assert!(again.get("approved").is_none() && again.get("started_by").is_none());
    }

    #[test]
    fn only_a_blocked_run_with_its_values_kept_awaits_approval() {
        let mut run: Run = serde_json::from_value(serde_json::json!({
            "id": "r", "workflow_id": "w", "workflow_name": "W", "version": 1, "status": "blocked",
            "dry_run": false, "trigger": "t", "trigger_key": null, "item_id": null, "chain": [],
            "steps": [], "error": null, "started_at": "2026-01-02T03:04:05Z", "finished_at": null,
            "resume": { "job": "", "step": 2, "ctx": {} },
        }))
        .unwrap();
        assert_eq!(run.awaiting_approval().map(|r| r.step), Some(2));
        run.status = RunStatus::Waiting;
        assert!(run.awaiting_approval().is_none());
        run.status = RunStatus::Blocked;
        run.resume.as_mut().unwrap().job = "job-1".into();
        assert!(
            run.awaiting_approval().is_none(),
            "a job's resume isn't an approval"
        );
    }
}
