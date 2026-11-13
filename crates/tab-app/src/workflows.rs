//! Workflows: listing, checking, saving, turning on and off, running, and
//! answering a run that waits on the person.

use chrono::Utc;
use tab_core::EngineEvent;
use tab_core::workflow::{self, Issue, Run, SavedBy, Workflow};

use crate::{Backend, err};

type Result<T> = std::result::Result<T, String>;

impl Backend {
    fn workflows_changed(&self, id: &str) {
        let _ = self.events.send(EngineEvent::WorkflowsChanged {
            id: id.into(),
            by: "user".into(),
        });
    }

    pub fn workflows(&self) -> Result<Vec<Workflow>> {
        self.store.workflows().map_err(err)
    }

    pub fn workflow_templates(&self) -> Vec<Workflow> {
        workflow::templates()
    }

    /// What's wrong with `w`, against the connectors and saved workflows.
    pub async fn workflow_validate(&self, w: &Workflow) -> Result<Vec<Issue>> {
        let catalog = self.with_engine(async |e| Ok(e.connectors())).await?;
        let saved = self.store.workflows().map_err(err)?;
        let families = self.store.model_family_ids().map_err(err)?;
        Ok(workflow::validate(w, &catalog, &saved, &families))
    }

    /// Saves the person's edits. A workflow with errors is saved turned off.
    pub async fn workflow_save(&self, w: Workflow) -> Result<(Workflow, Vec<Issue>)> {
        let catalog = self.with_engine(async |e| Ok(e.connectors())).await?;
        let saved = self.store.workflows().map_err(err)?;
        let families = self.store.model_family_ids().map_err(err)?;
        let (w, issues) = workflow::prepare_save(w, SavedBy::Person, &catalog, &saved, &families);
        let w = self
            .store
            .workflow_save(&w, "user", Utc::now())
            .map_err(err)?;
        self.workflows_changed(&w.id);
        Ok((w, issues))
    }

    /// Turns a workflow on or off. Turning on refuses one with errors.
    pub async fn workflow_set_enabled(&self, id: &str, enabled: bool) -> Result<Workflow> {
        let w = self
            .store
            .workflow_get(id)
            .map_err(err)?
            .ok_or("no such workflow")?;
        if enabled {
            let mut check = w.clone();
            check.enabled = true;
            if let Some(i) = self
                .workflow_validate(&check)
                .await?
                .into_iter()
                .find(|i| i.error)
            {
                return Err(i.message);
            }
        }
        let w = self
            .store
            .workflow_set_enabled(id, enabled)
            .map_err(err)?
            .ok_or("no such workflow")?;
        self.workflows_changed(&w.id);
        Ok(w)
    }

    pub fn workflow_delete(&self, id: &str) -> Result<()> {
        self.store.workflow_delete(id).map_err(err)?;
        self.workflows_changed(id);
        Ok(())
    }

    pub fn workflow_runs(&self, workflow_id: Option<&str>, limit: usize) -> Result<Vec<Run>> {
        self.store
            .workflow_runs(workflow_id, limit.min(500))
            .map_err(err)
    }

    /// Runs a workflow now: dry (nothing changes) or for real.
    pub async fn workflow_run(
        &self,
        w: Workflow,
        item_id: Option<String>,
        dry_run: bool,
    ) -> Result<Run> {
        self.with_engine(async |e| e.run_workflow(w, item_id.as_deref(), dry_run).await)
            .await
    }

    /// The person's answer to a run blocked on a step.
    pub async fn workflow_run_answer(&self, run_id: &str, approve: bool) -> Result<Run> {
        self.with_engine(async |e| e.answer_workflow_run(run_id, approve).await)
            .await
    }
}
