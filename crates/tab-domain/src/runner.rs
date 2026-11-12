//! Runners and jobs: who does work that needs a machine.
//!
//! A **runner** is anything connected to a hub that can do work: the app on
//! one of a person's computers, or (on paid tab cloud) a runner tab starts in
//! the cloud with that person's own harness setup. Each says what it can do
//! in [`RunnerCaps`].
//!
//! A **job** is a piece of work a workflow hands to a runner, such as an
//! agent session on a repo. The workflow writes the job and pauses; runners
//! that can do it race for the job's lease ([`job_lease`]); the hub grants it
//! to one, with a fencing token; the winner renews while it works and writes
//! the result. If it goes quiet, the lease expires and another runner can
//! take the job, until its attempts or its deadline run out.
//!
//! The same leases keep two runners from running one workflow trigger twice
//! ([`workflow_run_lease`]) and from polling one connection twice
//! ([`connection_lease`]). Everything here is pure: the callers pass `now`.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

pub use crate::families::Candidate;

/// How long a job lease lasts before it must be renewed.
pub const JOB_LEASE: Duration = Duration::seconds(90);
/// Renew this often while working (well inside [`JOB_LEASE`]).
pub const JOB_RENEW: Duration = Duration::seconds(30);
/// How long a workflow trigger stays claimed: long enough for the run's row
/// to reach every other runner through sync, after which the run's own
/// unique key keeps it from running again.
pub const WORKFLOW_RUN_LEASE: Duration = Duration::minutes(10);
/// A connection's poller renews its lease this often; the lease lasts 3×.
pub const CONNECTION_RENEW: Duration = Duration::seconds(60);
/// A job's default deadline.
pub const DEFAULT_JOB_MINUTES: u32 = 24 * 60;
/// Attempts a job gets before it fails, by default.
pub const DEFAULT_JOB_ATTEMPTS: u32 = 2;

/// The lease that lets one runner start a workflow for one trigger.
pub fn workflow_run_lease(workflow_id: &str, trigger_key: &str) -> String {
    format!("workflow-run:{workflow_id}:{trigger_key}")
}

/// The lease a runner holds while it works a job.
pub fn job_lease(job_id: &str) -> String {
    format!("job:{job_id}")
}

/// The lease a runner holds while it polls a connection.
pub fn connection_lease(connection: &str) -> String {
    format!("connection:{connection}")
}

/// Where a connection runs. A person picks this per connection, even with a
/// cloud account: a connection can stay on one computer.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunsOn {
    /// Only on the computer it was set up on. Its sign-in never leaves it.
    #[default]
    ThisComputer,
    /// In tab cloud, so it keeps running while every computer is off. Its
    /// sign-in is kept encrypted in the hub.
    Cloud,
}

/// What kind of runner.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunnerKind {
    #[default]
    Computer,
    Cloud,
}

/// What a runner can do, published on its runner record so jobs can be
/// matched to it.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RunnerCaps {
    /// The device id it says hello with.
    pub id: String,
    /// Shown to the person: "Vincent's MacBook", "tab cloud".
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub kind: RunnerKind,
    /// Harness ids it can run (`claude_code`, `opencode`, `codex`, `pi`).
    #[serde(default)]
    pub harnesses: Vec<String>,
    /// Repos it has checked out, as `owner/name`. A cloud runner clones
    /// instead, so this is empty for it.
    #[serde(default)]
    pub repos: Vec<String>,
    /// Connections that run here.
    #[serde(default)]
    pub connections: Vec<String>,
    /// When it last said it was alive.
    #[serde(default)]
    pub seen_at: Option<DateTime<Utc>>,
}

impl RunnerCaps {
    /// Whether it has been heard from recently enough to count as awake.
    pub fn awake(&self, now: DateTime<Utc>) -> bool {
        self.seen_at
            .is_some_and(|t| now - t < Duration::seconds(3 * 60))
    }
}

/// Where a job may run.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Placement {
    /// Any computer, or the cloud.
    Anywhere,
    /// Only the person's computers.
    #[default]
    Computers,
    /// Only a tab cloud runner (paid plans).
    Cloud,
    /// A computer if one takes it within `minutes`, then the cloud too.
    PreferComputer { minutes: u32 },
}

/// The work a job asks for.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct JobSpec {
    /// What the agent is asked to do.
    pub prompt: String,
    /// `owner/name`, when the work is on a repo.
    #[serde(default)]
    pub repo: String,
    /// The harness to use; empty means the runner's default.
    #[serde(default)]
    pub harness: String,
    /// The model to ask for; empty means the harness's default.
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub placement: Placement,
    /// The model family the step asked for, when it pinned neither a
    /// harness nor a model. Empty otherwise.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub family: String,
    /// That family's candidates, in order, as they were when the job was
    /// handed out.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<Candidate>,
}

impl JobSpec {
    /// Whether it pins a harness or a model, which wins over a family.
    pub fn pinned(&self) -> bool {
        !self.harness.is_empty() || !self.model.is_empty()
    }

    /// What a runner with `harnesses` installed tries, in order: the pinned
    /// harness and model; else the family's candidates it has; else the
    /// runner's default (one empty candidate). Empty when it has none of a
    /// family's harnesses.
    pub fn plan(&self, harnesses: &[String]) -> Vec<Candidate> {
        if self.pinned() || self.candidates.is_empty() {
            return vec![Candidate::new(&self.harness, &self.model)];
        }
        self.candidates
            .iter()
            .filter(|c| harnesses.contains(&c.harness))
            .cloned()
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum JobState {
    /// Waiting for a runner to claim it.
    Waiting,
    /// A runner holds its lease and is working on it.
    Running {
        runner: String,
        token: u64,
        started_at: DateTime<Utc>,
        /// The chat it runs in, so the person can watch it.
        #[serde(default)]
        session: Option<String>,
    },
    Done {
        runner: String,
        result: JobResult,
        finished_at: DateTime<Utc>,
    },
    Failed {
        error: String,
        finished_at: DateTime<Utc>,
    },
}

/// What came of a job.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct JobResult {
    /// The agent's last word on it.
    #[serde(default)]
    pub summary: String,
    /// A branch it pushed.
    #[serde(default)]
    pub branch: Option<String>,
    /// A pull request it opened.
    #[serde(default)]
    pub pr_url: Option<String>,
    /// What the agent's session spent, when the runner knows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<JobUsage>,
}

/// What a job's agent session spent, reported by the runner that ran it
/// (usage counters themselves never leave that device).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct JobUsage {
    pub billing: crate::workflow::Billing,
    /// What was billed; zero on a subscription.
    pub cost_usd: f64,
    /// On a subscription: the part of the plan's allowance it used, 0.0
    /// to 1.0, when the harness reports its plan windows.
    pub plan_share: f64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// The harness and model that ran, which a family or a default decided.
    pub harness: String,
    pub model: String,
    /// Time the session spent waiting on the person.
    pub human_wait_ms: u64,
    /// Times the runner moved on to the family's next model.
    pub fallbacks: u32,
}

/// One job, as stored and synced.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Job {
    pub id: String,
    /// The workflow run waiting on it, and its step.
    pub run_id: String,
    pub step_id: String,
    pub workflow_id: String,
    pub spec: JobSpec,
    pub state: JobState,
    /// Claims made on it so far.
    #[serde(default)]
    pub attempts: u32,
    pub max_attempts: u32,
    pub created_at: DateTime<Utc>,
    /// After this it fails instead of waiting longer.
    pub deadline: DateTime<Utc>,
    /// What a cloud runner's agent needed and didn't have: a connection
    /// that isn't allowed in tab cloud, a tool that's off there, a sign-in
    /// to redo. The person's computers show these with one-click fixes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing: Vec<CloudGap>,
}

/// Something a job in tab cloud needed and didn't have.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CloudGap {
    /// The connection's id (`linear`).
    pub connection: String,
    /// Its name, as the person sees it (`Linear`).
    #[serde(default)]
    pub name: String,
    /// The tool it tried, where it was one tool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    pub why: GapKind,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GapKind {
    /// Connected on a computer, not allowed in tab cloud jobs.
    NotInCloud,
    /// Connected on a computer, and it can only run there (a local
    /// program, a private address).
    OnlyOnComputer,
    /// Allowed in cloud jobs, but this tool isn't.
    ToolOff,
    /// Its cloud sign-in stopped working.
    SignIn,
}

impl CloudGap {
    /// Adds `gap` to `all` unless it's there already.
    pub fn add(all: &mut Vec<CloudGap>, gap: CloudGap) {
        if !all
            .iter()
            .any(|g| g.connection == gap.connection && g.tool == gap.tool && g.why == gap.why)
        {
            all.push(gap);
        }
    }
}

/// Why a runner may not take a job, for the job's status line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotForYou {
    Finished,
    PastDeadline,
    OutOfAttempts,
    /// Held by a runner whose lease is still live.
    Taken,
    CloudNotAllowed,
    ComputersNotAllowed,
    /// A computer still has first refusal.
    NotYet,
    NoHarness(String),
    NoRepo(String),
}

impl Job {
    /// Whether `runner` may claim this job now. A running job whose lease
    /// expired counts as waiting: `lease_live` says whether its holder's
    /// lease is still live.
    pub fn can_take(
        &self,
        runner: &RunnerCaps,
        lease_live: bool,
        now: DateTime<Utc>,
    ) -> Result<(), NotForYou> {
        match &self.state {
            JobState::Done { .. } | JobState::Failed { .. } => return Err(NotForYou::Finished),
            JobState::Running { runner: r, .. } if lease_live && r != &runner.id => {
                return Err(NotForYou::Taken);
            }
            _ => {}
        }
        if now >= self.deadline {
            return Err(NotForYou::PastDeadline);
        }
        let reclaim = matches!(&self.state, JobState::Running { runner: r, .. } if r == &runner.id);
        if !reclaim && self.attempts >= self.max_attempts {
            return Err(NotForYou::OutOfAttempts);
        }
        match (self.spec.placement, runner.kind) {
            (Placement::Computers, RunnerKind::Cloud) => return Err(NotForYou::CloudNotAllowed),
            (Placement::Cloud, RunnerKind::Computer) => {
                return Err(NotForYou::ComputersNotAllowed);
            }
            (Placement::PreferComputer { minutes }, RunnerKind::Cloud)
                if now < self.created_at + Duration::minutes(i64::from(minutes)) =>
            {
                return Err(NotForYou::NotYet);
            }
            _ => {}
        }
        if !self.spec.harness.is_empty() && !runner.harnesses.contains(&self.spec.harness) {
            return Err(NotForYou::NoHarness(self.spec.harness.clone()));
        }
        if self.spec.plan(&runner.harnesses).is_empty() {
            return Err(NotForYou::NoHarness(format!(
                "a harness in the {} family",
                self.spec.family
            )));
        }
        if runner.harnesses.is_empty() {
            return Err(NotForYou::NoHarness("any agent".into()));
        }
        // A cloud runner clones; a computer needs the repo checked out.
        if !self.spec.repo.is_empty()
            && runner.kind == RunnerKind::Computer
            && !runner
                .repos
                .iter()
                .any(|r| r.eq_ignore_ascii_case(&self.spec.repo))
        {
            return Err(NotForYou::NoRepo(self.spec.repo.clone()));
        }
        Ok(())
    }

    /// The job after `runner` won its lease with `token`.
    pub fn claimed(&self, runner: &str, token: u64, now: DateTime<Utc>) -> Job {
        let mut j = self.clone();
        let same = matches!(&self.state, JobState::Running { runner: r, token: t, .. } if r == runner && *t == token);
        if !same {
            j.attempts += 1;
        }
        j.state = JobState::Running {
            runner: runner.into(),
            token,
            started_at: now,
            session: match &self.state {
                JobState::Running { session, .. } if same => session.clone(),
                _ => None,
            },
        };
        j
    }

    /// Whether a result from `runner` holding `token` may be written: only
    /// the current holder's, so a runner whose lease expired and was taken
    /// can't overwrite the new holder's work.
    pub fn accepts_result_from(&self, runner: &str, token: u64) -> bool {
        matches!(&self.state, JobState::Running { runner: r, token: t, .. } if r == runner && *t == token)
    }

    /// Whether the job has ended, one way or another.
    pub fn finished(&self) -> bool {
        matches!(self.state, JobState::Done { .. } | JobState::Failed { .. })
    }

    /// Why no runner will take it any more, if so: past its deadline, or
    /// out of attempts with nobody holding it.
    pub fn given_up(&self, lease_live: bool, now: DateTime<Utc>) -> Option<String> {
        if self.finished() {
            return None;
        }
        if now >= self.deadline {
            return Some("No runner finished it before its deadline.".into());
        }
        let holding = matches!(self.state, JobState::Running { .. }) && lease_live;
        if !holding && self.attempts >= self.max_attempts {
            return Some(format!(
                "Tried {} times; each runner stopped before finishing.",
                self.attempts
            ));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(min: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000, 0).unwrap() + Duration::minutes(min)
    }

    fn job(placement: Placement) -> Job {
        Job {
            id: "j1".into(),
            run_id: "r1".into(),
            step_id: "fix".into(),
            workflow_id: "w1".into(),
            spec: JobSpec {
                prompt: "Fix the failing test".into(),
                repo: "acme/api".into(),
                harness: String::new(),
                model: String::new(),
                placement,
                ..Default::default()
            },
            state: JobState::Waiting,
            attempts: 0,
            max_attempts: 2,
            created_at: t(0),
            deadline: t(60),
            missing: Vec::new(),
        }
    }

    fn laptop() -> RunnerCaps {
        RunnerCaps {
            id: "laptop".into(),
            kind: RunnerKind::Computer,
            harnesses: vec!["claude_code".into()],
            repos: vec!["Acme/API".into()],
            ..Default::default()
        }
    }

    fn cloud() -> RunnerCaps {
        RunnerCaps {
            id: "cloud".into(),
            kind: RunnerKind::Cloud,
            harnesses: vec!["claude_code".into(), "codex".into()],
            ..Default::default()
        }
    }

    #[test]
    fn lease_keys_name_what_they_guard() {
        assert_eq!(
            workflow_run_lease("w", "ingested:i:1"),
            "workflow-run:w:ingested:i:1"
        );
        assert_eq!(job_lease("j"), "job:j");
        assert_eq!(connection_lease("github"), "connection:github");
    }

    #[test]
    fn placement_decides_who_may_take_a_job() {
        let now = t(1);
        let j = job(Placement::Computers);
        assert_eq!(j.can_take(&laptop(), false, now), Ok(()));
        assert_eq!(
            j.can_take(&cloud(), false, now),
            Err(NotForYou::CloudNotAllowed)
        );

        let j = job(Placement::Cloud);
        assert_eq!(
            j.can_take(&laptop(), false, now),
            Err(NotForYou::ComputersNotAllowed)
        );
        assert_eq!(j.can_take(&cloud(), false, now), Ok(()));

        let j = job(Placement::Anywhere);
        assert!(j.can_take(&laptop(), false, now).is_ok());
        assert!(j.can_take(&cloud(), false, now).is_ok());
    }

    #[test]
    fn a_computer_gets_first_refusal_when_preferred() {
        let j = job(Placement::PreferComputer { minutes: 10 });
        assert_eq!(j.can_take(&cloud(), false, t(9)), Err(NotForYou::NotYet));
        assert!(j.can_take(&laptop(), false, t(9)).is_ok());
        assert!(j.can_take(&cloud(), false, t(10)).is_ok());
    }

    #[test]
    fn a_computer_needs_the_harness_and_the_repo_and_the_cloud_clones() {
        let now = t(1);
        let mut j = job(Placement::Anywhere);
        let mut bare = laptop();
        bare.repos.clear();
        assert_eq!(
            j.can_take(&bare, false, now),
            Err(NotForYou::NoRepo("acme/api".into()))
        );
        assert!(j.can_take(&cloud(), false, now).is_ok());

        j.spec.harness = "codex".into();
        assert_eq!(
            j.can_take(&laptop(), false, now),
            Err(NotForYou::NoHarness("codex".into()))
        );
        assert!(j.can_take(&cloud(), false, now).is_ok());

        let mut none = laptop();
        none.harnesses.clear();
        j.spec.harness.clear();
        assert!(matches!(
            j.can_take(&none, false, now),
            Err(NotForYou::NoHarness(_))
        ));

        j.spec.repo.clear();
        assert!(j.can_take(&bare, false, now).is_ok());
    }

    #[test]
    fn a_held_job_is_taken_until_its_lease_lapses() {
        let j = job(Placement::Anywhere).claimed("laptop", 7, t(1));
        assert_eq!(j.attempts, 1);
        assert_eq!(j.can_take(&cloud(), true, t(2)), Err(NotForYou::Taken));
        // The holder may claim again (a renewal), and that's not an attempt.
        assert!(j.can_take(&laptop(), true, t(2)).is_ok());
        assert_eq!(j.claimed("laptop", 7, t(2)).attempts, 1);
        // Lapsed: anyone eligible may take it, and that is an attempt.
        assert!(j.can_take(&cloud(), false, t(5)).is_ok());
        let j = j.claimed("cloud", 8, t(5));
        assert_eq!(j.attempts, 2);
        // Out of attempts once it lapses again.
        assert_eq!(
            j.can_take(&laptop(), false, t(9)),
            Err(NotForYou::OutOfAttempts)
        );
        assert!(j.given_up(false, t(9)).is_some());
        assert!(j.given_up(true, t(9)).is_none());
    }

    #[test]
    fn only_the_current_holder_may_write_the_result() {
        let j = job(Placement::Anywhere)
            .claimed("laptop", 7, t(1))
            .claimed("cloud", 8, t(3));
        assert!(!j.accepts_result_from("laptop", 7));
        assert!(!j.accepts_result_from("cloud", 7));
        assert!(j.accepts_result_from("cloud", 8));
    }

    #[test]
    fn finished_and_late_jobs_are_left_alone() {
        let mut j = job(Placement::Anywhere);
        assert_eq!(
            j.can_take(&laptop(), false, t(60)),
            Err(NotForYou::PastDeadline)
        );
        assert!(j.given_up(false, t(60)).unwrap().contains("deadline"));
        j.state = JobState::Done {
            runner: "laptop".into(),
            result: JobResult::default(),
            finished_at: t(2),
        };
        assert!(j.finished());
        assert_eq!(j.can_take(&laptop(), false, t(3)), Err(NotForYou::Finished));
        assert!(j.given_up(false, t(90)).is_none());
    }

    #[test]
    fn a_runner_counts_as_awake_for_three_minutes() {
        let mut r = laptop();
        assert!(!r.awake(t(0)));
        r.seen_at = Some(t(0));
        assert!(r.awake(t(2)));
        assert!(!r.awake(t(3)));
    }

    fn family_job() -> Job {
        let mut j = job(Placement::Anywhere);
        j.spec.family = "deep".into();
        j.spec.candidates = vec![
            Candidate::new("codex", "gpt-6.1-sol"),
            Candidate::new("claude_code", "claude-opus-5-5"),
            Candidate::new("pi", ""),
        ];
        j
    }

    #[test]
    fn a_runner_may_take_a_family_job_when_any_candidate_fits() {
        let now = t(1);
        let j = family_job();
        assert!(j.can_take(&laptop(), false, now).is_ok());
        assert!(j.can_take(&cloud(), false, now).is_ok());
        let mut opencode = laptop();
        opencode.harnesses = vec!["opencode".into()];
        assert_eq!(
            j.can_take(&opencode, false, now),
            Err(NotForYou::NoHarness("a harness in the deep family".into()))
        );
    }

    #[test]
    fn a_runner_tries_the_family_in_order_skipping_what_it_lacks() {
        let j = family_job();
        assert_eq!(
            j.spec.plan(&laptop().harnesses),
            [Candidate::new("claude_code", "claude-opus-5-5")]
        );
        let all: Vec<String> = ["pi", "claude_code", "codex"].map(String::from).into();
        let plan: Vec<_> = j.spec.plan(&all).into_iter().map(|c| c.harness).collect();
        assert_eq!(plan, ["codex", "claude_code", "pi"]);
    }

    #[test]
    fn a_pinned_harness_or_model_wins_over_a_family() {
        let mut j = family_job();
        j.spec.harness = "codex".into();
        assert_eq!(
            j.spec.plan(&cloud().harnesses),
            [Candidate::new("codex", "")]
        );
        assert_eq!(
            j.can_take(&laptop(), false, t(1)),
            Err(NotForYou::NoHarness("codex".into()))
        );
        j.spec.harness.clear();
        j.spec.model = "claude-sonnet-5-5".into();
        assert_eq!(
            j.spec.plan(&laptop().harnesses),
            [Candidate::new("", "claude-sonnet-5-5")]
        );
        // No pin and no family: the runner's default.
        assert_eq!(
            job(Placement::Anywhere).spec.plan(&[]),
            [Candidate::default()]
        );
    }

    #[test]
    fn jobs_from_before_families_still_parse() {
        let old = r#"{"id":"j1","run_id":"r1","step_id":"fix","workflow_id":"w1",
            "spec":{"prompt":"Fix it","repo":"acme/api","harness":"","model":"","placement":{"type":"anywhere"}},
            "state":{"state":"waiting"},"attempts":0,"max_attempts":2,
            "created_at":"2027-01-15T08:00:00Z","deadline":"2027-01-15T09:00:00Z"}"#;
        let j: Job = serde_json::from_str(old).unwrap();
        assert!(j.spec.family.is_empty() && j.spec.candidates.is_empty());
        // And a job without a family writes no family fields, for old runners.
        let v = serde_json::to_value(&j).unwrap();
        assert!(v["spec"].get("family").is_none() && v["spec"].get("candidates").is_none());
        let back: Job =
            serde_json::from_value(serde_json::to_value(family_job()).unwrap()).unwrap();
        assert_eq!(back, family_job());
    }

    #[test]
    fn jobs_and_settings_round_trip_as_json() {
        let j = job(Placement::PreferComputer { minutes: 15 }).claimed("laptop", 3, t(1));
        let back: Job = serde_json::from_value(serde_json::to_value(&j).unwrap()).unwrap();
        assert_eq!(back, j);
        assert_eq!(
            serde_json::to_value(RunsOn::ThisComputer).unwrap(),
            "this_computer"
        );
        assert_eq!(
            serde_json::from_str::<Placement>(r#"{"type":"cloud"}"#).unwrap(),
            Placement::Cloud
        );
    }
}
