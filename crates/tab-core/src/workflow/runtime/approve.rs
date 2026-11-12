//! A run blocked on a destructive or command step carries on once the
//! person approves that step, on that run only.
//!
//! Consent stays with the person: [`Runtime::answer_blocked`] is reached
//! only from a button in the app (the `workflow_run_answer` command), never
//! from MCP or an agent. Approving lets one step of one run go ahead with
//! the exact inputs the person saw; it never touches the workflow's
//! `allow_destructive` or `allow_commands`, so the next run asks again.

use std::sync::Arc;

use chrono::Utc;

use super::{Run, RunStatus, Runtime, StepStatus, Workflow};
use crate::engine::EngineEvent;
use crate::leases;
use tab_domain::runner::{WORKFLOW_RUN_LEASE, workflow_run_lease};
use tab_domain::workflow::{Intervention, InterventionKind, StepApproval};

impl Runtime {
    /// The workflow as it was when `run` ran.
    pub(super) fn version_that_ran(&self, run: &Run) -> anyhow::Result<Workflow> {
        let store = self.host.store();
        Ok(
            match store
                .workflow_versions(&run.workflow_id)?
                .into_iter()
                .find(|(v, ..)| *v == run.version)
            {
                Some((.., w)) => w,
                None => store
                    .workflow_get(&run.workflow_id)?
                    .ok_or_else(|| anyhow::anyhow!("its workflow was deleted"))?,
            },
        )
    }

    /// The person's answer to a run blocked on a step. Approving runs that
    /// step with the inputs it was blocked with and carries on from there;
    /// declining ends the run. Only for a person's action in the app.
    pub async fn answer_blocked(
        self: &Arc<Self>,
        run_id: &str,
        approve: bool,
    ) -> anyhow::Result<Run> {
        let store = self.host.store();
        let not_waiting = || anyhow::anyhow!("This run isn't waiting for an OK any more.");
        let first = store.workflow_run_get(run_id)?.ok_or_else(not_waiting)?;
        first.awaiting_approval().ok_or_else(not_waiting)?;
        // Two devices answering at once: one of them does.
        let key = workflow_run_lease(&first.workflow_id, &format!("approve:{run_id}"));
        match leases::claim(&self.host.leases(), &key, WORKFLOW_RUN_LEASE).await {
            Ok(c) if !c.won => anyhow::bail!("It's being answered on another device."),
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(run = %run_id, error = %e, "couldn't reach the hub to claim an approval; answering here")
            }
        }
        let out = self.answer(run_id, approve).await;
        let _ = leases::release(&self.host.leases(), &key).await;
        out
    }

    async fn answer(self: &Arc<Self>, run_id: &str, approve: bool) -> anyhow::Result<Run> {
        let store = self.host.store();
        let not_waiting = || anyhow::anyhow!("This run isn't waiting for an OK any more.");
        // Read again under the lease: it may have been answered meanwhile.
        let mut run = store.workflow_run_get(run_id)?.ok_or_else(not_waiting)?;
        let resume = run.awaiting_approval().cloned().ok_or_else(not_waiting)?;
        let mut blocked = run
            .steps
            .last()
            .filter(|s| s.status == StepStatus::Blocked)
            .cloned()
            .ok_or_else(not_waiting)?;
        let now = Utc::now();
        if !blocked
            .interventions
            .iter()
            .any(|i| i.kind == InterventionKind::Approval)
        {
            blocked.interventions.push(Intervention {
                kind: InterventionKind::Approval,
                at: now,
            });
        }
        run.resume = None;

        if !approve {
            let name = if blocked.label.is_empty() {
                blocked.id.clone()
            } else {
                blocked.label.clone()
            };
            if let Some(sr) = run.steps.last_mut() {
                sr.error = Some("You declined this step.".into());
                sr.interventions = blocked.interventions;
            }
            run.status = RunStatus::Stopped;
            run.error = Some(format!("You declined {name}, so the run ended there."));
            run.finished_at = Some(now);
            store.workflow_run_update(&run)?;
            self.host
                .publish(EngineEvent::WorkflowRun { run: run.clone() });
            return Ok(run);
        }

        let w = self.version_that_ran(&run)?;
        anyhow::ensure!(
            w.steps.get(resume.step).is_some_and(|s| s.id == blocked.id),
            "The workflow changed since this run, so it can't carry on. Run it again instead."
        );
        run.steps.pop();
        run.approved.push(StepApproval {
            step: blocked.id.clone(),
            input: blocked.input.clone(),
            at: now,
        });
        run.status = RunStatus::Running;
        run.error = None;
        run.finished_at = None;
        store.workflow_run_update(&run)?;
        self.host
            .publish(EngineEvent::WorkflowRun { run: run.clone() });

        let _slot = self.slots.acquire().await?;
        let at = run.steps.len();
        self.execute(&w, &mut run, resume.ctx, resume.step).await;
        // The approval belongs to the step that ran.
        if let Some(sr) = run.steps.get_mut(at).filter(|s| s.id == blocked.id) {
            let mut all = blocked.interventions;
            all.append(&mut sr.interventions);
            sr.interventions = all;
        }
        if run.status != RunStatus::Waiting {
            run.finished_at = Some(Utc::now());
        }
        store.workflow_run_update(&run)?;
        self.host
            .publish(EngineEvent::WorkflowRun { run: run.clone() });
        Ok(run)
    }
}
