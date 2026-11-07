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
}
