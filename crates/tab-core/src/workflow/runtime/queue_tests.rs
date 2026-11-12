//! Approving a blocked step, and agents starting workflows.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::Utc;
use serde_json::{Value, json};

use super::*;
use crate::connector::{Connector, ConnectorContext};
use crate::leases::InMemory;
use crate::model::{AgentKind, ConnectorStatus};
use crate::workflow::{InterventionKind, Step};
use tab_domain::runner::Placement;

/// GitHub stand-in that writes down the actions it's asked to run.
#[derive(Default)]
struct Acting(Mutex<Vec<(String, Value)>>);

#[async_trait]
impl Connector for Acting {
    fn id(&self) -> &str {
        "github"
    }
    async fn run(&self, _ctx: ConnectorContext) -> anyhow::Result<()> {
        Ok(())
    }
    async fn act(&self, action: &str, input: &Value) -> anyhow::Result<Value> {
        self.0.lock().unwrap().push((action.into(), input.clone()));
        Ok(json!({ "merged": true }))
    }
}

struct Host2 {
    store: Store,
    leases: Arc<InMemory>,
    github: Arc<Acting>,
}

#[async_trait]
impl Host for Host2 {
    fn store(&self) -> &Store {
        &self.store
    }
    fn catalog(&self) -> Vec<ConnectorInfo> {
        crate::connectors::registry()
            .into_iter()
            .map(|f| ConnectorInfo {
                descriptor: f.descriptor(),
                enabled: true,
                status: ConnectorStatus::Starting,
            })
            .collect()
    }
    fn connector(&self, id: &str) -> Option<Arc<dyn Connector>> {
        (id == "github").then(|| self.github.clone() as Arc<dyn Connector>)
    }
    async fn tab_action(
        &self,
        _action: &str,
        _input: &Value,
        _chain: &[String],
        _workflow: &str,
    ) -> anyhow::Result<(Value, Option<String>)> {
        anyhow::bail!("not in this test")
    }
    fn publish(&self, _e: EngineEvent) {}
    fn leases(&self) -> Arc<dyn Leases> {
        self.leases.clone()
    }
}

fn setup() -> (Arc<Runtime>, Arc<Host2>) {
    let host = Arc::new(Host2 {
        store: Store::in_memory().unwrap(),
        leases: InMemory::new("laptop"),
        github: Arc::default(),
    });
    (Runtime::new(host.clone()), host)
}

fn step(id: &str, kind: StepKind) -> Step {
    Step {
        id: id.into(),
        label: String::new(),
        when: Vec::new(),
        continue_on_error: false,
        kind,
    }
}

fn set(values: Value) -> StepKind {
    StepKind::Set {
        values: values.as_object().unwrap().clone(),
    }
}

/// Merges PR 7, then notes that it did.
fn merging(store: &Store, enabled: bool) -> Workflow {
    let input = json!({ "repo": "acme/shop", "number": "7", "method": "squash" });
    let w = Workflow {
        name: "Merge when green".into(),
        enabled,
        trigger: Trigger::Manual,
        steps: vec![
            step("first", set(json!({ "pr": "7" }))),
            step(
                "merge",
                StepKind::Action {
                    connector: "github".into(),
                    action: "pr.merge".into(),
                    input: input.as_object().unwrap().clone(),
                },
            ),
            step("after", set(json!({ "merged": "{{steps.merge.merged}}" }))),
        ],
        ..Default::default()
    };
    store.workflow_save(&w, "user", Utc::now()).unwrap()
}

fn simple(store: &Store, enabled: bool) -> Workflow {
    let w = Workflow {
        name: "Note it".into(),
        enabled,
        trigger: Trigger::Manual,
        steps: vec![step("only", set(json!({ "x": 1 })))],
        ..Default::default()
    };
    store.workflow_save(&w, "user", Utc::now()).unwrap()
}

#[tokio::test]
async fn a_blocked_step_runs_once_the_person_approves_it_and_the_run_carries_on() {
    let (rt, host) = setup();
    let w = merging(&host.store, true);
    let run = rt.run_now(w.clone(), None, false).await.unwrap();
    assert_eq!(run.status, RunStatus::Blocked);
    assert_eq!(run.awaiting_approval().map(|r| r.step), Some(1));
    assert!(host.github.0.lock().unwrap().is_empty());

    let done = rt.answer_blocked(&run.id, true).await.unwrap();
    assert_eq!(done.status, RunStatus::Succeeded, "{:?}", done.error);
    assert!(done.resume.is_none() && done.finished_at.is_some());
    let ids: Vec<(&str, StepStatus)> = done
        .steps
        .iter()
        .map(|s| (s.id.as_str(), s.status))
        .collect();
    assert_eq!(
        ids,
        [
            ("first", StepStatus::Ok),
            ("merge", StepStatus::Ok),
            ("after", StepStatus::Ok)
        ]
    );
    assert_eq!(done.steps[2].output["merged"], true);
    assert_eq!(done.approved.len(), 1);
    assert!(
        done.steps[1]
            .interventions
            .iter()
            .any(|i| i.kind == InterventionKind::Approval)
    );
    assert_eq!(host.github.0.lock().unwrap().len(), 1);
    assert_eq!(host.store.workflow_run_get(&run.id).unwrap().unwrap(), done);

    // Only that run: the workflow still doesn't allow it, and the next run asks again.
    assert!(
        !host
            .store
            .workflow_get(&w.id)
            .unwrap()
            .unwrap()
            .allow_destructive
    );
    let next = rt.run_now(w, None, false).await.unwrap();
    assert_eq!(next.status, RunStatus::Blocked);
    assert_eq!(host.github.0.lock().unwrap().len(), 1);

    // Answering twice does nothing more.
    assert!(rt.answer_blocked(&run.id, true).await.is_err());
    assert_eq!(host.github.0.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn declining_a_blocked_step_ends_the_run_and_runs_nothing() {
    let (rt, host) = setup();
    let w = merging(&host.store, true);
    let run = rt.run_now(w, None, false).await.unwrap();
    let out = rt.answer_blocked(&run.id, false).await.unwrap();
    assert_eq!(out.status, RunStatus::Stopped);
    assert!(out.resume.is_none() && out.awaiting_approval().is_none());
    assert!(out.error.unwrap().contains("declined"));
    assert_eq!(out.steps.len(), 2);
    assert!(host.github.0.lock().unwrap().is_empty());
    assert!(rt.answer_blocked(&run.id, true).await.is_err(), "it's over");
    assert!(host.github.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn an_approval_covers_only_the_inputs_the_person_saw() {
    let (rt, host) = setup();
    let mut w = merging(&host.store, true);
    // The PR number comes from the run's values now.
    if let StepKind::Action { input, .. } = &mut w.steps[1].kind {
        input.insert("number".into(), json!("{{steps.first.pr}}"));
    }
    let w = host.store.workflow_save(&w, "user", Utc::now()).unwrap();
    let mut run = rt.run_now(w, None, false).await.unwrap();
    assert_eq!(run.steps[1].input["number"], "7");
    // Something changes the kept values after the person saw the step.
    run.resume.as_mut().unwrap().ctx["steps"]["first"]["pr"] = json!("8");
    host.store.workflow_run_update(&run).unwrap();
    let out = rt.answer_blocked(&run.id, true).await.unwrap();
    assert_eq!(out.status, RunStatus::Blocked, "asks again for #8");
    assert_eq!(out.steps.last().unwrap().input["number"], "8");
    assert!(host.github.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn an_agent_can_only_start_a_workflow_the_person_turned_on() {
    let (rt, host) = setup();
    let off = simple(&host.store, false);
    let e = rt
        .run_for_agent(&off.id, None, Some("chat-1"), Utc::now())
        .await
        .unwrap_err();
    assert!(e.to_string().contains("turned off"), "{e}");
    assert!(
        host.store
            .workflow_runs(Some(&off.id), 10)
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn an_agent_started_run_records_its_chat_and_a_repeat_gets_the_same_run() {
    let (rt, host) = setup();
    let w = simple(&host.store, true);
    let now = Utc::now();
    let first = rt
        .run_for_agent(&w.id, None, Some("chat-1"), now)
        .await
        .unwrap();
    assert!(!first.repeat);
    assert_eq!(first.run.status, RunStatus::Succeeded);
    assert_eq!(first.run.started_by.as_deref(), Some("chat-1"));
    assert_eq!(first.run.trigger, "by an agent");
    assert!(first.run.chain.is_empty() && first.run.caused_by.is_none());

    let again = rt
        .run_for_agent(&w.id, None, Some("chat-1"), now)
        .await
        .unwrap();
    assert!(again.repeat);
    assert_eq!(again.run.id, first.run.id);
    assert_eq!(host.store.workflow_runs(Some(&w.id), 10).unwrap().len(), 1);

    // Another chat is another request.
    let other = rt
        .run_for_agent(&w.id, None, Some("chat-2"), now)
        .await
        .unwrap();
    assert!(!other.repeat && other.run.id != first.run.id);
}

#[tokio::test]
async fn agent_runs_count_against_the_workflows_hourly_limit() {
    let (rt, host) = setup();
    let mut w = simple(&host.store, true);
    w.limits.max_runs_per_hour = 1;
    let w = host.store.workflow_save(&w, "user", Utc::now()).unwrap();
    let now = Utc::now();
    let ok = rt.run_for_agent(&w.id, None, Some("a"), now).await.unwrap();
    assert_eq!(ok.run.status, RunStatus::Succeeded);
    let held = rt.run_for_agent(&w.id, None, Some("b"), now).await.unwrap();
    assert_eq!(held.run.status, RunStatus::Throttled);
}

#[tokio::test]
async fn destructive_and_command_steps_stay_blocked_for_an_agent_unless_the_person_allowed_them() {
    let (rt, host) = setup();
    let w = merging(&host.store, true);
    let run = rt
        .run_for_agent(&w.id, None, Some("a"), Utc::now())
        .await
        .unwrap()
        .run;
    assert_eq!(run.status, RunStatus::Blocked);
    assert!(host.github.0.lock().unwrap().is_empty());

    let cmd = Workflow {
        name: "Build".into(),
        enabled: true,
        trigger: Trigger::Manual,
        steps: vec![step(
            "build",
            StepKind::Command {
                program: "sh".into(),
                args: vec!["-c".into(), "echo hi".into()],
                folder: String::new(),
                network: vec![],
                env: Default::default(),
                image: String::new(),
                timeout_secs: 10,
            },
        )],
        ..Default::default()
    };
    let cmd = host.store.workflow_save(&cmd, "user", Utc::now()).unwrap();
    let run = rt
        .run_for_agent(&cmd.id, None, Some("a"), Utc::now())
        .await
        .unwrap()
        .run;
    assert_eq!(run.status, RunStatus::Blocked);
    assert_eq!(run.steps[0].status, StepStatus::Blocked);

    let mut allowed = w.clone();
    allowed.allow_destructive = true;
    let allowed = host
        .store
        .workflow_save(&allowed, "user", Utc::now())
        .unwrap();
    let run = rt
        .run_for_agent(&allowed.id, None, Some("b"), Utc::now())
        .await
        .unwrap()
        .run;
    assert_eq!(run.status, RunStatus::Succeeded, "{:?}", run.error);
    assert_eq!(host.github.0.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn an_agent_a_workflow_started_carries_its_chain_so_the_workflow_cant_loop_through_it() {
    let (rt, host) = setup();
    // A workflow whose agent step starts a job.
    let w = Workflow {
        name: "Fix and rerun".into(),
        enabled: true,
        trigger: Trigger::Manual,
        steps: vec![step(
            "fix",
            StepKind::Agent {
                prompt: "Fix it".into(),
                repo: "acme/api".into(),
                harness: "codex".into(),
                model: String::new(),
                family: String::new(),
                placement: Placement::Anywhere,
                deadline_minutes: 0,
            },
        )],
        ..Default::default()
    };
    let w = host.store.workflow_save(&w, "user", Utc::now()).unwrap();
    let parent = rt.run_now(w.clone(), None, false).await.unwrap();
    assert_eq!(parent.status, RunStatus::Waiting);
    let job = parent.resume.as_ref().unwrap().job.clone();
    let chat = host
        .store
        .session_create_keyed(
            &format!("job:{job}"),
            AgentKind::Codex,
            "job",
            ".",
            Utc::now(),
        )
        .unwrap();

    // The job's agent tries to start the same workflow: a loop.
    let looped = rt
        .run_for_agent(&w.id, None, Some(&chat.id), Utc::now())
        .await
        .unwrap()
        .run;
    assert_eq!(looped.status, RunStatus::LoopStopped);
    assert_eq!(looped.chain, std::slice::from_ref(&w.id));

    // Another workflow is fine, and remembers what led to it.
    let other = simple(&host.store, true);
    let ran = rt
        .run_for_agent(&other.id, None, Some(&chat.id), Utc::now())
        .await
        .unwrap()
        .run;
    assert_eq!(ran.status, RunStatus::Succeeded);
    assert_eq!(ran.chain, std::slice::from_ref(&w.id));
    assert_eq!(ran.caused_by.as_deref(), Some(parent.id.as_str()));
    assert_eq!(ran.started_by.as_deref(), Some(chat.id.as_str()));
}
