//! Agents starting workflows (`tab_run_workflow` over MCP).
//!
//! An agent may start only a workflow the person turned on, and the run
//! goes through every guardrail a trigger does: loops and chain length,
//! dedup, the per-workflow and global rate limits, and destructive and
//! command steps stay blocked unless the person allowed them on that
//! workflow. An agent that a workflow run started (an agent step's job)
//! passes that run's chain on, so a workflow can't loop through an agent.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use super::{Run, Runtime, Start, base_ctx};
use crate::model::QueueItem;

/// The same agent asking for the same workflow on the same item inside
/// this window gets the run it already started.
const AGENT_DEDUP_SECS: i64 = 10 * 60;

/// What came of an agent asking for a run.
#[derive(Debug, Clone)]
pub struct AgentRun {
    pub run: Run,
    /// It had already started this; `run` is that run.
    pub repeat: bool,
}

impl Runtime {
    /// The chain and run behind an agent chat: a workflow job's chat
    /// carries its run's chain with that run's workflow on the end.
    fn agent_origin(&self, session: &str) -> anyhow::Result<(Vec<String>, Option<String>)> {
        let store = self.host.store();
        let Some(job) = store
            .session_key(session)?
            .and_then(|k| k.strip_prefix("job:").map(str::to_string))
        else {
            return Ok((Vec::new(), None));
        };
        let Some(job) = store.job_get(&job)? else {
            return Ok((Vec::new(), None));
        };
        let Some(run) = store.workflow_run_get(&job.run_id)? else {
            // Its run isn't here (yet): still name the workflow, so it can't
            // start itself again.
            return Ok((vec![job.workflow_id], Some(job.run_id)));
        };
        let mut chain = run.chain;
        chain.push(run.workflow_id);
        Ok((chain, Some(run.id)))
    }

    /// Starts workflow `workflow_id` for an agent (session `session`, if
    /// tab started it), on `item` if given, and waits for it like a manual
    /// run does.
    pub async fn run_for_agent(
        self: &Arc<Self>,
        workflow_id: &str,
        item: Option<QueueItem>,
        session: Option<&str>,
        now: DateTime<Utc>,
    ) -> anyhow::Result<AgentRun> {
        let store = self.host.store();
        let w = store.workflow_get(workflow_id)?.ok_or_else(|| {
            anyhow::anyhow!("no workflow {workflow_id}; tab_list_workflows lists them")
        })?;
        anyhow::ensure!(
            w.enabled,
            "\"{}\" is turned off. Agents can only start workflows the person turned on; ask them to turn it on in the Workflows tab.",
            w.name
        );
        let session = session.filter(|s| !s.is_empty());
        let (chain, caused_by) = match session {
            Some(s) => self.agent_origin(s)?,
            None => (Vec::new(), None),
        };
        let slot = now.timestamp().div_euclid(AGENT_DEDUP_SECS);
        let key = format!(
            "agent:{}:{}:{slot}",
            session.unwrap_or("-"),
            item.as_ref().map_or("-", |i| i.id.as_str())
        );
        if let Some(run) = store.workflow_run_by_key(&w.id, &key)? {
            return Ok(AgentRun { run, repeat: true });
        }
        let ctx = base_ctx(
            &w,
            json!({ "type": "agent", "change": item.as_ref().map(|_| "agent") }),
            item.as_ref(),
            Value::Null,
        );
        let start = Start {
            trigger: match &item {
                Some(i) => format!("by an agent on {}", i.event.title),
                None => "by an agent".into(),
            },
            key: Some(key),
            item_id: item.map(|i| i.id),
            chain,
            ctx,
            dry_run: false,
            started_by: session.map(str::to_string),
            caused_by,
        };
        let run = self.clone().begin(&w, start).await?;
        Ok(AgentRun { run, repeat: false })
    }
}
