//! Workflows on queue items: each item's one workflow status for its row
//! and header, and an item's runs with what each links to (the agent chat
//! of its agent step, the chat or run that started it, the step waiting
//! for the person's OK).
//!
//! The status itself is decided by the pure
//! [`tab_domain::workflow::item_status`]; this adds what only this device
//! knows: the jobs' chats and whether they're waiting on the person.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;

use crate::model::{Session, SessionStatus};
use crate::store::Store;
use crate::workflow::item_status::{
    ItemWorkflowState, ItemWorkflowStatus, RunAgent, item_statuses, run_state,
};
use crate::workflow::{Run, RunStatus, StepStatus};
use tab_domain::runner::{Job, JobState};

/// Whether a chat is waiting on the person: its status says so, or it has
/// an approval open (`pending` asks the agent hub).
fn needs_you(s: &Session, pending: &dyn Fn(&str) -> bool) -> bool {
    s.status == SessionStatus::NeedsInput || pending(&s.id)
}

/// A job's chat on this device, if it has one.
fn job_session(store: &Store, job: &Job) -> Option<Session> {
    let by_state = match &job.state {
        JobState::Running {
            session: Some(id), ..
        } => store.session_get(id).ok().flatten(),
        _ => None,
    };
    by_state.or_else(|| {
        store
            .session_for_key(&format!("job:{}", job.id))
            .ok()
            .flatten()
    })
}

/// What the agent of each waiting run is doing, by run id.
fn agents_of(
    store: &Store,
    runs: &[Run],
    pending: &dyn Fn(&str) -> bool,
) -> anyhow::Result<HashMap<String, RunAgent>> {
    let waiting: Vec<String> = runs
        .iter()
        .filter(|r| r.status == RunStatus::Waiting)
        .map(|r| r.id.clone())
        .collect();
    let mut out = HashMap::new();
    if waiting.is_empty() {
        return Ok(out);
    }
    let jobs: HashMap<String, Job> = store
        .jobs_for_runs(&waiting)?
        .into_iter()
        .map(|j| (j.id.clone(), j))
        .collect();
    for r in runs.iter().filter(|r| r.status == RunStatus::Waiting) {
        let Some(job) = r.resume.as_ref().and_then(|x| jobs.get(&x.job)) else {
            continue;
        };
        let session = job_session(store, job);
        out.insert(
            r.id.clone(),
            RunAgent {
                job: job.id.clone(),
                needs_you: session.as_ref().is_some_and(|s| needs_you(s, pending)),
                session: session.map(|s| s.id),
            },
        );
    }
    Ok(out)
}

/// Every item's workflow status, for the queue. One read of the runs that
/// can show, one of their jobs.
pub fn statuses(
    store: &Store,
    pending: &dyn Fn(&str) -> bool,
    now: DateTime<Utc>,
) -> anyhow::Result<Vec<ItemWorkflowStatus>> {
    let runs = store.workflow_runs_on_items(now)?;
    let agents = agents_of(store, &runs, pending)?;
    Ok(item_statuses(&runs, &agents, now))
}

/// An agent step's chat.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AgentLink {
    pub step_id: String,
    pub job: String,
    /// The chat on this device; absent when it ran elsewhere and isn't here.
    pub session: Option<String>,
    pub status: Option<SessionStatus>,
    pub needs_you: bool,
}

/// The chat that started a run.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ChatLink {
    pub session: String,
    pub title: String,
    /// The task the chat is on, to open it there.
    pub item_id: Option<String>,
    /// A workflow job's chat, opened on its own.
    pub job: Option<String>,
}

/// The run that led to this one.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RunLink {
    pub id: String,
    pub workflow_name: String,
    pub item_id: Option<String>,
    /// False when an item change linked them: it's the run of that
    /// workflow just before this one, which is most likely it.
    pub exact: bool,
}

/// The step a blocked run waits on the person's OK for.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Approval {
    pub step_id: String,
    pub label: String,
    /// Why it was blocked, in words.
    pub reason: Option<String>,
    /// What it would do: the inputs it runs with if approved.
    pub input: Value,
}

/// A run with its links, for an item's list of runs and the Runs panel.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RunView {
    pub run: Run,
    /// What the run shows on its item now, if anything.
    pub state: Option<ItemWorkflowState>,
    pub agents: Vec<AgentLink>,
    pub started_by: Option<ChatLink>,
    pub caused_by: Option<RunLink>,
    pub approval: Option<Approval>,
}

fn view(
    store: &Store,
    run: Run,
    jobs: &[Job],
    newest: bool,
    pending: &dyn Fn(&str) -> bool,
    now: DateTime<Utc>,
) -> RunView {
    let agents: Vec<AgentLink> = jobs
        .iter()
        .filter(|j| j.run_id == run.id)
        .map(|j| {
            let s = job_session(store, j);
            AgentLink {
                step_id: j.step_id.clone(),
                job: j.id.clone(),
                status: s.as_ref().map(|s| s.status),
                needs_you: s.as_ref().is_some_and(|s| needs_you(s, pending)),
                session: s.map(|s| s.id),
            }
        })
        .collect();
    let waiting_agent = run
        .resume
        .as_ref()
        .and_then(|r| agents.iter().find(|a| a.job == r.job));
    let mut by_run = HashMap::new();
    if run.status == RunStatus::Waiting
        && let Some(a) = waiting_agent
    {
        by_run.insert(
            run.id.clone(),
            RunAgent {
                job: a.job.clone(),
                session: a.session.clone(),
                needs_you: a.needs_you,
            },
        );
    }
    let state = run_state(&run, &by_run, newest, now);
    let started_by = run.started_by.as_deref().and_then(|id| {
        let s = store.session_get(id).ok().flatten()?;
        let job = store
            .session_key(id)
            .ok()
            .flatten()
            .and_then(|k| k.strip_prefix("job:").map(str::to_string));
        Some(ChatLink {
            session: s.id,
            title: s.title,
            item_id: s.item_id,
            job,
        })
    });
    let caused_by = match &run.caused_by {
        Some(id) => store.workflow_run_get(id).ok().flatten().map(|r| RunLink {
            id: r.id,
            workflow_name: r.workflow_name,
            item_id: r.item_id,
            exact: true,
        }),
        None => run.chain.last().and_then(|wf| {
            store
                .workflow_runs(Some(wf), 50)
                .ok()?
                .into_iter()
                .find(|r| !r.dry_run && r.started_at <= run.started_at)
                .map(|r| RunLink {
                    id: r.id,
                    workflow_name: r.workflow_name,
                    item_id: r.item_id,
                    exact: false,
                })
        }),
    };
    let approval = run.awaiting_approval().and_then(|_| {
        let s = run
            .steps
            .last()
            .filter(|s| s.status == StepStatus::Blocked)?;
        Some(Approval {
            step_id: s.id.clone(),
            label: s.label.clone(),
            reason: s.error.clone(),
            input: s.input.clone(),
        })
    });
    RunView {
        run,
        state,
        agents,
        started_by,
        caused_by,
        approval,
    }
}

/// An item's runs, newest first, with their links.
pub fn item_runs(
    store: &Store,
    item_id: &str,
    limit: usize,
    pending: &dyn Fn(&str) -> bool,
    now: DateTime<Utc>,
) -> anyhow::Result<Vec<RunView>> {
    let runs = store.workflow_runs_for_item(item_id, limit)?;
    let ids: Vec<String> = runs.iter().map(|r| r.id.clone()).collect();
    let jobs = store.jobs_for_runs(&ids)?;
    let mut seen = std::collections::HashSet::new();
    Ok(runs
        .into_iter()
        .map(|r| {
            // Newest first, so the first counted run of each workflow is
            // its newest.
            let counted = !matches!(r.status, RunStatus::Stopped | RunStatus::LoopStopped);
            let newest = counted && seen.insert(r.workflow_id.clone());
            view(store, r, &jobs, newest, pending, now)
        })
        .collect())
}

/// One run with its links.
pub fn run_view(
    store: &Store,
    run_id: &str,
    pending: &dyn Fn(&str) -> bool,
    now: DateTime<Utc>,
) -> anyhow::Result<Option<RunView>> {
    let Some(run) = store.workflow_run_get(run_id)? else {
        return Ok(None);
    };
    let jobs = store.jobs_for_runs(std::slice::from_ref(&run.id))?;
    let newest = match &run.item_id {
        Some(item) => store
            .workflow_runs_for_item(item, 200)?
            .into_iter()
            .find(|r| {
                r.workflow_id == run.workflow_id
                    && !matches!(r.status, RunStatus::Stopped | RunStatus::LoopStopped)
            })
            .is_some_and(|r| r.id == run.id),
        None => false,
    };
    Ok(Some(view(store, run, &jobs, newest, pending, now)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::AgentKind;
    use serde_json::json;

    fn run(id: &str, status: &str, extra: Value) -> Run {
        let mut v = json!({
            "id": id, "workflow_id": format!("w-{id}"), "workflow_name": format!("W {id}"), "version": 1,
            "status": status, "dry_run": false, "trigger": "t", "trigger_key": null, "item_id": "item",
            "chain": [], "steps": [], "error": null, "started_at": Utc::now(), "finished_at": null,
        });
        for (k, x) in extra.as_object().unwrap() {
            v[k] = x.clone();
        }
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn a_workflows_agent_asking_you_something_puts_its_item_waiting_for_you() {
        let s = Store::in_memory().unwrap();
        let now = Utc::now();
        let waiting = run(
            "a",
            "waiting",
            json!({ "resume": { "job": "j1", "step": 0, "ctx": {} } }),
        );
        s.workflow_run_insert(&waiting).unwrap();
        let job: Job = serde_json::from_value(json!({
            "id": "j1", "run_id": "a", "step_id": "fix", "workflow_id": "w-a",
            "spec": { "prompt": "Fix it" }, "state": { "state": "waiting" }, "attempts": 0,
            "max_attempts": 3, "created_at": now, "deadline": now + chrono::Duration::hours(1),
        }))
        .unwrap();
        s.job_put(&job, now).unwrap();
        let none = |_: &str| false;
        let out = statuses(&s, &none, now).unwrap();
        assert_eq!(out[0].state, ItemWorkflowState::WaitingOnAgent);

        // A runner starts the agent, and it asks something.
        let chat = s
            .session_create_keyed("job:j1", AgentKind::ClaudeCode, "job", ".", now)
            .unwrap();
        s.session_update(&chat.id, Some(SessionStatus::NeedsInput), None, now)
            .unwrap();
        let out = statuses(&s, &none, now).unwrap();
        assert_eq!(out[0].state, ItemWorkflowState::WaitingForYou);
        assert_eq!(out[0].session.as_deref(), Some(chat.id.as_str()));

        // The list says which chat to open.
        let views = item_runs(&s, "item", 10, &none, now).unwrap();
        assert_eq!(
            views[0].agents[0].session.as_deref(),
            Some(chat.id.as_str())
        );
        assert!(views[0].agents[0].needs_you);
        assert_eq!(views[0].state, Some(ItemWorkflowState::WaitingForYou));
    }

    #[test]
    fn a_runs_view_names_what_started_it_and_the_step_waiting_for_your_ok() {
        let s = Store::in_memory().unwrap();
        let now = Utc::now();
        let chat = s
            .session_create_keyed(
                "chat:x",
                AgentKind::ClaudeCode,
                "Tidy the backlog",
                ".",
                now,
            )
            .unwrap();
        s.workflow_run_insert(&run("parent", "succeeded", json!({ "finished_at": now })))
            .unwrap();
        let blocked = run(
            "child",
            "blocked",
            json!({
                "finished_at": now, "started_by": chat.id, "caused_by": "parent",
                "resume": { "job": "", "step": 0, "ctx": {} },
                "steps": [{ "id": "merge", "label": "Merge", "status": "blocked", "input": { "number": 7 },
                            "output": null, "error": "Merge is destructive.", "ms": 0 }],
            }),
        );
        s.workflow_run_insert(&blocked).unwrap();
        let none = |_: &str| false;
        let v = run_view(&s, "child", &none, now).unwrap().unwrap();
        assert_eq!(v.state, Some(ItemWorkflowState::WaitingForYou));
        let a = v.approval.unwrap();
        assert_eq!(
            (a.step_id.as_str(), a.input["number"].as_i64()),
            ("merge", Some(7))
        );
        assert_eq!(v.started_by.unwrap().title, "Tidy the backlog");
        let c = v.caused_by.unwrap();
        assert!(c.exact && c.id == "parent" && c.workflow_name == "W parent");
    }
}
