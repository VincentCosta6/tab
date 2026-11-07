//! Workflows, their saved versions, and their runs.

use std::sync::atomic::Ordering;

use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};

use super::{Store, parse_ts, ts};
use crate::workflow::{Run, RunStatus, Workflow};

/// A saved definition: `(version, saved_by, saved_at, workflow)`.
pub type WorkflowVersion = (u32, String, DateTime<Utc>, Workflow);

/// Statuses that did work and so count against a workflow's hourly limit.
const COUNTED: &str = "'running','succeeded','stopped','failed','blocked'";

impl Store {
    pub fn workflows_rev(&self) -> u64 {
        self.workflows_rev.load(Ordering::Relaxed)
    }

    pub(crate) fn workflows_bump(&self) {
        self.workflows_rev.fetch_add(1, Ordering::Relaxed);
    }

    pub fn workflows(&self) -> anyhow::Result<Vec<Workflow>> {
        let conn = self.conn();
        let mut st =
            conn.prepare("SELECT definition, enabled FROM workflows ORDER BY created_at, id")?;
        let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, bool>(1)?)))?;
        let mut out = Vec::new();
        for row in rows {
            let (def, enabled) = row?;
            match serde_json::from_str::<Workflow>(&def) {
                Ok(mut w) => {
                    w.enabled = enabled;
                    out.push(w);
                }
                Err(e) => tracing::warn!(error = %e, "skipping a workflow that no longer parses"),
            }
        }
        Ok(out)
    }

    pub fn workflow_get(&self, id: &str) -> anyhow::Result<Option<Workflow>> {
        let row: Option<(String, bool)> = self
            .conn()
            .query_row(
                "SELECT definition, enabled FROM workflows WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        Ok(match row {
            Some((def, enabled)) => {
                let mut w: Workflow = serde_json::from_str(&def)?;
                w.enabled = enabled;
                Some(w)
            }
            None => None,
        })
    }

    /// Inserts or updates a workflow, keeping the old definition as a
    /// version. Assigns an id to a new one. Returns what was stored.
    pub fn workflow_save(
        &self,
        w: &Workflow,
        saved_by: &str,
        now: DateTime<Utc>,
    ) -> anyhow::Result<Workflow> {
        let mut w = w.clone();
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let prev: Option<(u32, String)> = if w.id.is_empty() {
            None
        } else {
            tx.query_row(
                "SELECT version, created_by FROM workflows WHERE id = ?1",
                [&w.id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
        };
        if w.id.is_empty() {
            w.id = uuid::Uuid::new_v4().to_string();
        }
        w.version = prev.as_ref().map_or(1, |(v, _)| v + 1);
        w.updated_at = Some(now);
        if let Some((_, by)) = &prev {
            w.created_by = by.clone();
        } else if w.created_by.is_empty() {
            w.created_by = saved_by.to_string();
        }
        let def = serde_json::to_string(&w)?;
        tx.execute(
            "INSERT INTO workflows (id, name, enabled, definition, version, created_by, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
             ON CONFLICT(id) DO UPDATE SET name = excluded.name, enabled = excluded.enabled,
                 definition = excluded.definition, version = excluded.version, updated_at = excluded.updated_at",
            params![w.id, w.name, w.enabled, def, w.version, w.created_by, ts(now)],
        )?;
        tx.execute(
            "INSERT OR REPLACE INTO workflow_versions (workflow_id, version, definition, saved_by, saved_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![w.id, w.version, def, saved_by, ts(now)],
        )?;
        tx.commit()?;
        drop(conn);
        self.workflows_bump();
        Ok(w)
    }

    pub fn workflow_set_enabled(
        &self,
        id: &str,
        enabled: bool,
    ) -> anyhow::Result<Option<Workflow>> {
        self.conn().execute(
            "UPDATE workflows SET enabled = ?2 WHERE id = ?1",
            params![id, enabled],
        )?;
        self.workflows_bump();
        self.workflow_get(id)
    }

    pub fn workflow_delete(&self, id: &str) -> anyhow::Result<()> {
        let conn = self.conn();
        conn.execute("DELETE FROM workflows WHERE id = ?1", [id])?;
        conn.execute("DELETE FROM workflow_runs WHERE workflow_id = ?1", [id])?;
        drop(conn);
        self.workflows_bump();
        Ok(())
    }

    /// Saved definitions, newest first.
    pub fn workflow_versions(&self, id: &str) -> anyhow::Result<Vec<WorkflowVersion>> {
        let conn = self.conn();
        let mut st = conn.prepare(
            "SELECT version, saved_by, saved_at, definition FROM workflow_versions
             WHERE workflow_id = ?1 ORDER BY version DESC LIMIT 50",
        )?;
        let rows = st.query_map([id], |r| {
            Ok((
                r.get::<_, u32>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (v, by, at, def) = row?;
            out.push((v, by, parse_ts(&at)?, serde_json::from_str(&def)?));
        }
        Ok(out)
    }

    /// Stores a new run. Returns false if a run with the same trigger key
    /// already exists for this workflow (the trigger was a duplicate).
    pub fn workflow_run_insert(&self, run: &Run) -> anyhow::Result<bool> {
        let n = self.conn().execute(
            "INSERT OR IGNORE INTO workflow_runs (id, workflow_id, version, status, dry_run, trigger_key, item_id, body, started_at, finished_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                run.id, run.workflow_id, run.version, run.status.as_str(), run.dry_run, run.trigger_key,
                run.item_id, serde_json::to_string(run)?, ts(run.started_at), run.finished_at.map(ts)
            ],
        )?;
        Ok(n == 1)
    }

    pub fn workflow_run_update(&self, run: &Run) -> anyhow::Result<()> {
        self.conn().execute(
            "UPDATE workflow_runs SET status = ?2, body = ?3, finished_at = ?4 WHERE id = ?1",
            params![
                run.id,
                run.status.as_str(),
                serde_json::to_string(run)?,
                run.finished_at.map(ts)
            ],
        )?;
        Ok(())
    }

    pub fn workflow_run_seen(&self, workflow_id: &str, key: &str) -> anyhow::Result<bool> {
        Ok(self
            .conn()
            .query_row(
                "SELECT 1 FROM workflow_runs WHERE workflow_id = ?1 AND trigger_key = ?2",
                params![workflow_id, key],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// Runs that did work since `since`, not counting dry runs.
    pub fn workflow_runs_since(
        &self,
        workflow_id: &str,
        since: DateTime<Utc>,
    ) -> anyhow::Result<usize> {
        let n: i64 = self.conn().query_row(
            &format!(
                "SELECT COUNT(*) FROM workflow_runs WHERE workflow_id = ?1 AND started_at >= ?2
                   AND dry_run = 0 AND status IN ({COUNTED})"
            ),
            params![workflow_id, ts(since)],
            |r| r.get(0),
        )?;
        Ok(n as usize)
    }

    pub fn workflow_last_run_for_item(
        &self,
        workflow_id: &str,
        item_id: &str,
    ) -> anyhow::Result<Option<DateTime<Utc>>> {
        let t: Option<String> = self.conn().query_row(
            &format!(
                "SELECT MAX(started_at) FROM workflow_runs WHERE workflow_id = ?1 AND item_id = ?2
                   AND dry_run = 0 AND status IN ({COUNTED})"
            ),
            params![workflow_id, item_id],
            |r| r.get(0),
        )?;
        t.map(|t| parse_ts(&t)).transpose().map_err(Into::into)
    }

    /// Newest first. With no workflow, every workflow's runs.
    pub fn workflow_runs(
        &self,
        workflow_id: Option<&str>,
        limit: usize,
    ) -> anyhow::Result<Vec<Run>> {
        let conn = self.conn();
        let mut st = conn.prepare(
            "SELECT body FROM workflow_runs WHERE (?1 IS NULL OR workflow_id = ?1)
             ORDER BY started_at DESC LIMIT ?2",
        )?;
        let rows = st.query_map(params![workflow_id, limit as i64], |r| {
            r.get::<_, String>(0)
        })?;
        let mut out = Vec::new();
        for row in rows {
            if let Ok(run) = serde_json::from_str::<Run>(&row?) {
                out.push(run);
            }
        }
        Ok(out)
    }

    /// Runs left `running` by a previous app session never finished.
    pub fn workflow_runs_abandon(&self, now: DateTime<Utc>) -> anyhow::Result<()> {
        let stale: Vec<Run> = {
            let conn = self.conn();
            let mut st = conn.prepare("SELECT body FROM workflow_runs WHERE status = 'running'")?;
            st.query_map([], |r| r.get::<_, String>(0))?
                .filter_map(|r| r.ok())
                .filter_map(|b| serde_json::from_str(&b).ok())
                .collect()
        };
        for mut run in stale {
            run.status = RunStatus::Failed;
            run.error = Some("tab closed before the run finished".into());
            run.finished_at = Some(now);
            self.workflow_run_update(&run)?;
        }
        Ok(())
    }

    pub fn workflow_runs_prune(&self, before: DateTime<Utc>) -> anyhow::Result<usize> {
        Ok(self.conn().execute(
            "DELETE FROM workflow_runs WHERE started_at < ?1",
            [ts(before)],
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::templates;

    #[test]
    fn saves_versions_and_dedups_runs() {
        let s = Store::in_memory().unwrap();
        let now = Utc::now();
        let rev = s.workflows_rev();
        let w = s.workflow_save(&templates()[0], "user", now).unwrap();
        assert_eq!((w.version, w.created_by.as_str()), (1, "template"));
        assert!(s.workflows_rev() > rev);
        let mut next = w.clone();
        next.name = "Renamed".into();
        let w2 = s.workflow_save(&next, "agent", now).unwrap();
        assert_eq!((w2.version, w2.id.as_str()), (2, w.id.as_str()));
        assert_eq!(s.workflow_versions(&w.id).unwrap().len(), 2);
        assert!(!s.workflows().unwrap()[0].enabled);
        s.workflow_set_enabled(&w.id, true).unwrap();
        assert!(s.workflow_get(&w.id).unwrap().unwrap().enabled);

        let run = Run {
            id: "r1".into(),
            workflow_id: w.id.clone(),
            workflow_name: w.name.clone(),
            version: 2,
            status: RunStatus::Succeeded,
            dry_run: false,
            trigger: "t".into(),
            trigger_key: Some("k".into()),
            item_id: Some("i".into()),
            chain: vec![],
            steps: vec![],
            error: None,
            started_at: now,
            finished_at: Some(now),
        };
        assert!(s.workflow_run_insert(&run).unwrap());
        let mut dup = run.clone();
        dup.id = "r2".into();
        assert!(!s.workflow_run_insert(&dup).unwrap(), "same trigger key");
        assert!(s.workflow_run_seen(&w.id, "k").unwrap());
        assert_eq!(
            s.workflow_runs_since(&w.id, now - chrono::Duration::hours(1))
                .unwrap(),
            1
        );
        assert!(s.workflow_last_run_for_item(&w.id, "i").unwrap().is_some());
        assert_eq!(s.workflow_runs(Some(&w.id), 10).unwrap()[0].id, "r1");
        s.workflow_delete(&w.id).unwrap();
        assert!(s.workflows().unwrap().is_empty());
    }
}
