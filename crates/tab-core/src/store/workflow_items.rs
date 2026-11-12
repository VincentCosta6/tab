//! Workflow runs by item, for the queue's workflow status and an item's
//! list of runs.

use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params, params_from_iter};

use super::{Store, ts};
use crate::workflow::Run;
use tab_domain::runner::Job;
use tab_domain::workflow::item_status::{FAILED_FOR, OPEN_FOR};

fn runs(rows: impl Iterator<Item = rusqlite::Result<String>>) -> Vec<Run> {
    rows.filter_map(|r| r.ok())
        .filter_map(|b| serde_json::from_str(&b).ok())
        .collect()
}

impl Store {
    /// Runs on items that may show a workflow status (see
    /// [`tab_domain::workflow::item_status`]): every real run on an item
    /// from the last two days, plus older ones still going or blocked
    /// with no newer run of their workflow on that item. Newest first.
    pub fn workflow_runs_on_items(&self, now: DateTime<Utc>) -> anyhow::Result<Vec<Run>> {
        let conn = self.conn();
        let mut st = conn.prepare(
            "SELECT body FROM workflow_runs r
             WHERE item_id IS NOT NULL AND dry_run = 0 AND (
               started_at >= ?1
               OR (status IN ('running', 'waiting') AND started_at >= ?2)
               OR (status = 'blocked' AND started_at >= ?2 AND NOT EXISTS (
                 SELECT 1 FROM workflow_runs n
                 WHERE n.item_id = r.item_id AND n.workflow_id = r.workflow_id
                   AND n.started_at > r.started_at AND n.dry_run = 0
                   AND n.status NOT IN ('stopped', 'loop_stopped'))))
             ORDER BY started_at DESC LIMIT 2000",
        )?;
        let rows = st.query_map(params![ts(now - FAILED_FOR * 2), ts(now - OPEN_FOR)], |r| {
            r.get::<_, String>(0)
        })?;
        Ok(runs(rows))
    }

    /// One item's real runs, newest first.
    pub fn workflow_runs_for_item(&self, item_id: &str, limit: usize) -> anyhow::Result<Vec<Run>> {
        let conn = self.conn();
        let mut st = conn.prepare(
            "SELECT body FROM workflow_runs WHERE item_id = ?1 AND dry_run = 0
             ORDER BY started_at DESC LIMIT ?2",
        )?;
        let rows = st.query_map(params![item_id, limit as i64], |r| r.get::<_, String>(0))?;
        Ok(runs(rows))
    }

    /// The run a trigger key started for a workflow, if any.
    pub fn workflow_run_by_key(&self, workflow_id: &str, key: &str) -> anyhow::Result<Option<Run>> {
        let body: Option<String> = self
            .conn()
            .query_row(
                "SELECT body FROM workflow_runs WHERE workflow_id = ?1 AND trigger_key = ?2",
                params![workflow_id, key],
                |r| r.get(0),
            )
            .optional()?;
        Ok(body.and_then(|b| serde_json::from_str(&b).ok()))
    }

    /// The jobs these runs handed to runners, oldest first.
    pub fn jobs_for_runs(&self, run_ids: &[String]) -> anyhow::Result<Vec<Job>> {
        let mut out = Vec::new();
        let conn = self.conn();
        for ids in run_ids.chunks(200) {
            let marks = vec!["?"; ids.len()].join(",");
            let mut st = conn.prepare(&format!(
                "SELECT body FROM jobs WHERE run_id IN ({marks}) ORDER BY created_at"
            ))?;
            out.extend(
                st.query_map(params_from_iter(ids), |r| r.get::<_, String>(0))?
                    .filter_map(|r| r.ok())
                    .filter_map(|b| serde_json::from_str::<Job>(&b).ok()),
            );
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::RunStatus;

    fn run(id: &str, workflow: &str, status: RunStatus, at: DateTime<Utc>) -> Run {
        Run {
            id: id.into(),
            workflow_id: workflow.into(),
            workflow_name: "W".into(),
            version: 1,
            status,
            dry_run: false,
            trigger: "t".into(),
            trigger_key: Some(format!("k-{id}")),
            item_id: Some("item".into()),
            chain: vec![],
            steps: vec![],
            error: None,
            started_at: at,
            finished_at: Some(at),
            runner: String::new(),
            resume: None,
            rerun_of: None,
            started_by: None,
            caused_by: None,
            approved: vec![],
        }
    }

    #[test]
    fn runs_on_items_keep_old_blocked_runs_only_until_a_newer_run_replaces_them() {
        let s = Store::in_memory().unwrap();
        let now = Utc::now();
        let days = |d| now - chrono::Duration::days(d);
        s.workflow_run_insert(&run("recent", "w1", RunStatus::Succeeded, days(1)))
            .unwrap();
        s.workflow_run_insert(&run("old-done", "w1", RunStatus::Succeeded, days(4)))
            .unwrap();
        s.workflow_run_insert(&run("old-going", "w2", RunStatus::Waiting, days(4)))
            .unwrap();
        s.workflow_run_insert(&run("old-ask", "w3", RunStatus::Blocked, days(5)))
            .unwrap();
        s.workflow_run_insert(&run("replaced", "w4", RunStatus::Blocked, days(5)))
            .unwrap();
        s.workflow_run_insert(&run("newer", "w4", RunStatus::Failed, days(4)))
            .unwrap();
        s.workflow_run_insert(&run("ancient", "w5", RunStatus::Running, days(9)))
            .unwrap();
        let mut dry = run("dry", "w1", RunStatus::Running, now);
        dry.dry_run = true;
        s.workflow_run_insert(&dry).unwrap();

        let ids: Vec<String> = s
            .workflow_runs_on_items(now)
            .unwrap()
            .into_iter()
            .map(|r| r.id)
            .collect();
        assert_eq!(ids, ["recent", "old-going", "old-ask"]);

        assert_eq!(s.workflow_runs_for_item("item", 3).unwrap().len(), 3);
        assert_eq!(
            s.workflow_run_by_key("w1", "k-recent").unwrap().unwrap().id,
            "recent"
        );
        assert!(s.workflow_run_by_key("w2", "k-recent").unwrap().is_none());
    }
}
