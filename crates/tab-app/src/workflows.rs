//! Workflows: listing, checking, saving, turning on and off, running, and
//! answering a run that waits on the person.

use chrono::Utc;
use tab_core::EngineEvent;
use tab_core::families::ModelFamily;
use tab_core::model::Session;

use crate::chat::Chat;
use tab_core::runner::RunnerCaps;
use tab_core::workflow::insights::{Factory, WorkflowInsights};
use tab_core::workflow::items::RunView;
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

    /// Turns a workflow on or off, or allows its destructive steps or
    /// commands. Turning on refuses one with errors.
    pub async fn workflow_set_enabled(
        &self,
        id: &str,
        enabled: bool,
        allow_destructive: Option<bool>,
        allow_commands: Option<bool>,
    ) -> Result<Workflow> {
        let mut w = self
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
        let allowances = allow_destructive.is_some_and(|a| a != w.allow_destructive)
            || allow_commands.is_some_and(|a| a != w.allow_commands);
        w.enabled = enabled;
        if let Some(a) = allow_destructive {
            w.allow_destructive = a;
        }
        if let Some(a) = allow_commands {
            w.allow_commands = a;
        }
        let w = if allowances {
            self.store
                .workflow_save(&w, "user", Utc::now())
                .map_err(err)?
        } else {
            self.store
                .workflow_set_enabled(id, enabled)
                .map_err(err)?
                .ok_or("no such workflow")?
        };
        self.workflows_changed(&w.id);
        Ok(w)
    }

    /// Model families agent steps can run on.
    pub fn model_families(&self) -> Result<Vec<ModelFamily>> {
        self.store.model_families().map_err(err)
    }

    /// Every runner the hub knows, whether each is awake, and whether it's
    /// this app.
    pub fn runners(&self) -> Result<Vec<(RunnerCaps, bool, bool)>> {
        let me = self.store.device_id().map_err(err)?;
        let now = Utc::now();
        Ok(self
            .store
            .runners()
            .map_err(err)?
            .into_iter()
            .map(|c| {
                let (awake, this_app) = (c.awake(now), c.id == me);
                (c, awake, this_app)
            })
            .collect())
    }

    /// One workflow's stats for the last `days` days and the span before.
    pub fn workflow_insights(
        &self,
        id: &str,
        days: u32,
        utc_offset_minutes: i32,
    ) -> Result<WorkflowInsights> {
        workflow::insights_store::workflow_insights(
            &self.store,
            id,
            days,
            utc_offset_minutes,
            Utc::now(),
        )
        .map_err(err)
    }

    /// Every workflow's stats and health, with hub-wide totals.
    pub fn workflow_factory(&self, days: u32, utc_offset_minutes: i32) -> Result<Factory> {
        workflow::insights_store::workflow_factory(
            &self.store,
            days,
            utc_offset_minutes,
            Utc::now(),
        )
        .map_err(err)
    }

    /// One run with what it links to.
    pub fn workflow_run_view(&self, run_id: &str) -> Result<Option<RunView>> {
        let agents = self.agents.clone();
        let pending = move |id: &str| !agents.pending_approvals(id).is_empty();
        workflow::items::run_view(&self.store, run_id, &pending, Utc::now()).map_err(err)
    }

    /// The builder assistant's chat for one workflow ("" for a new draft).
    pub fn workflow_chat_open(&self, workflow_id: &str) -> Result<Chat> {
        let session = self
            .store
            .session_for_key(&workflow::chat_key(workflow_id))
            .map_err(err)?;
        self.chat_of(session)
    }

    /// A message to the builder's assistant, with the workflow that's open.
    pub fn workflow_chat_send(&self, text: &str, open: Option<&Workflow>) -> Result<Session> {
        let text = text.trim();
        if text.is_empty() {
            return Err("message is empty".into());
        }
        if self.agents.config().kind.is_none() {
            return Err("Set up an agent first (Settings → Agent).".into());
        }
        let (title, brief) = workflow::chat_brief(open).map_err(err)?;
        let cwd = self.agents.workdir();
        let session = self
            .agents
            .session_for_key(
                &workflow::chat_key(open.map_or("", |w| w.id.as_str())),
                &title,
                &cwd,
                &brief,
            )
            .map_err(err)?;
        self.agents.send_with(&session.id, text, &[]).map_err(err)?;
        Ok(session)
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
