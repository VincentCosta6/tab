//! Jobs workflows hand to runners, the runners themselves, and the
//! connections that run in tab cloud. All three sync through the hub
//! (see [`tab_domain::runner`] for the rules).

use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};
use serde_json::{Map, Value};

use super::{Store, ts};
use tab_domain::runner::{Job, JobState, RunnerCaps};

fn state_name(s: &JobState) -> &'static str {
    match s {
        JobState::Waiting => "waiting",
        JobState::Running { .. } => "running",
        JobState::Done { .. } => "done",
        JobState::Failed { .. } => "failed",
    }
}

/// A connection set to run in tab cloud, as the cloud runner reads it.
#[derive(Debug, Clone, PartialEq)]
pub struct CloudConnection {
    pub id: String,
    pub enabled: bool,
    /// Settings without the secret fields.
    pub settings: Map<String, Value>,
}

impl Store {
    /// Inserts or replaces a job.
    pub fn job_put(&self, job: &Job, now: DateTime<Utc>) -> anyhow::Result<()> {
        self.conn().execute(
            "INSERT INTO jobs (id, run_id, workflow_id, state, body, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT (id) DO UPDATE SET state = excluded.state, body = excluded.body,
                 updated_at = excluded.updated_at",
            params![
                job.id,
                job.run_id,
                job.workflow_id,
                state_name(&job.state),
                serde_json::to_string(job)?,
                ts(job.created_at),
                ts(now)
            ],
        )?;
        Ok(())
    }

    pub fn job_get(&self, id: &str) -> anyhow::Result<Option<Job>> {
        let body: Option<String> = self
            .conn()
            .query_row("SELECT body FROM jobs WHERE id = ?1", [id], |r| r.get(0))
            .optional()?;
        Ok(body.and_then(|b| serde_json::from_str(&b).ok()))
    }

    /// Jobs that haven't ended, oldest first.
    pub fn jobs_open(&self) -> anyhow::Result<Vec<Job>> {
        self.jobs_where("state IN ('waiting', 'running')", 200)
    }

    /// Newest first.
    pub fn jobs_recent(&self, limit: usize) -> anyhow::Result<Vec<Job>> {
        let mut out = self.jobs_where("1", limit)?;
        out.reverse();
        Ok(out)
    }

    fn jobs_where(&self, cond: &str, limit: usize) -> anyhow::Result<Vec<Job>> {
        let conn = self.conn();
        let mut st = conn.prepare(&format!(
            "SELECT body FROM jobs WHERE {cond} ORDER BY created_at DESC LIMIT ?1"
        ))?;
        let rows = st.query_map([limit as i64], |r| r.get::<_, String>(0))?;
        let mut out: Vec<Job> = rows
            .filter_map(|r| r.ok())
            .filter_map(|b| serde_json::from_str(&b).ok())
            .collect();
        out.reverse();
        Ok(out)
    }

    /// Runs paused on a job, with the job's id.
    pub fn workflow_runs_waiting(&self) -> anyhow::Result<Vec<crate::workflow::Run>> {
        let conn = self.conn();
        let mut st = conn.prepare("SELECT body FROM workflow_runs WHERE status = 'waiting'")?;
        let out = st
            .query_map([], |r| r.get::<_, String>(0))?
            .filter_map(|r| r.ok())
            .filter_map(|b| serde_json::from_str(&b).ok())
            .collect();
        Ok(out)
    }

    pub fn workflow_run_get(&self, id: &str) -> anyhow::Result<Option<crate::workflow::Run>> {
        let body: Option<String> = self
            .conn()
            .query_row("SELECT body FROM workflow_runs WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .optional()?;
        Ok(body.and_then(|b| serde_json::from_str(&b).ok()))
    }

    /// Repos with a folder on this computer: `(owner/name, folder)`.
    pub fn repos_checked_out(&self) -> anyhow::Result<Vec<(String, String)>> {
        let conn = self.conn();
        let mut st = conn.prepare("SELECT repo, path FROM project_repos WHERE path IS NOT NULL")?;
        let out = st
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .filter_map(|r| r.ok())
            .filter(|(_, p)| std::path::Path::new(p).is_dir())
            .collect();
        Ok(out)
    }

    /// Says what this runner can do, and that it's awake.
    pub fn runner_put(&self, r: &RunnerCaps) -> anyhow::Result<()> {
        let seen = r.seen_at.unwrap_or_else(Utc::now);
        self.conn().execute(
            "INSERT INTO runners (id, body, seen_at) VALUES (?1, ?2, ?3)
             ON CONFLICT (id) DO UPDATE SET body = excluded.body, seen_at = excluded.seen_at",
            params![r.id, serde_json::to_string(r)?, ts(seen)],
        )?;
        Ok(())
    }

    pub fn runners(&self) -> anyhow::Result<Vec<RunnerCaps>> {
        let conn = self.conn();
        let mut st = conn.prepare("SELECT body FROM runners ORDER BY seen_at DESC")?;
        let out = st
            .query_map([], |r| r.get::<_, String>(0))?
            .filter_map(|r| r.ok())
            .filter_map(|b| serde_json::from_str(&b).ok())
            .collect();
        Ok(out)
    }

    /// Marks a connection as running in tab cloud with these (non-secret)
    /// settings, or, with `None`, as not running there any more.
    pub fn cloud_connection_set(
        &self,
        id: &str,
        settings: Option<&Map<String, Value>>,
        now: DateTime<Utc>,
    ) -> anyhow::Result<()> {
        match settings {
            Some(s) => self.conn().execute(
                "INSERT INTO cloud_connections (id, enabled, settings, updated_at) VALUES (?1, 1, ?2, ?3)
                 ON CONFLICT (id) DO UPDATE SET enabled = 1, settings = excluded.settings,
                     updated_at = excluded.updated_at",
                params![id, serde_json::to_string(s)?, ts(now)],
            )?,
            None => self
                .conn()
                .execute("DELETE FROM cloud_connections WHERE id = ?1", [id])?,
        };
        Ok(())
    }

    pub fn cloud_connections(&self) -> anyhow::Result<Vec<CloudConnection>> {
        let conn = self.conn();
        let mut st =
            conn.prepare("SELECT id, enabled, settings FROM cloud_connections ORDER BY id")?;
        let out = st
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, bool>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?
            .filter_map(|r| r.ok())
            .map(|(id, enabled, s)| CloudConnection {
                id,
                enabled,
                settings: serde_json::from_str(&s).unwrap_or_default(),
            })
            .collect();
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tab_domain::runner::{JobResult, JobSpec, Placement, RunnerKind};

    fn job(id: &str, at: DateTime<Utc>) -> Job {
        Job {
            id: id.into(),
            run_id: "r".into(),
            step_id: "fix".into(),
            workflow_id: "w".into(),
            spec: JobSpec {
                prompt: "Fix it".into(),
                placement: Placement::Anywhere,
                ..Default::default()
            },
            state: JobState::Waiting,
            attempts: 0,
            max_attempts: 2,
            created_at: at,
            deadline: at + chrono::Duration::hours(1),
            missing: Vec::new(),
            claimed_at: None,
            interventions: Vec::new(),
        }
    }

    #[test]
    fn jobs_round_trip_and_open_ones_are_listed_oldest_first() {
        let s = Store::in_memory().unwrap();
        let t0 = Utc::now();
        s.job_put(&job("b", t0 + chrono::Duration::seconds(1)), t0)
            .unwrap();
        s.job_put(&job("a", t0), t0).unwrap();
        let open: Vec<_> = s.jobs_open().unwrap().into_iter().map(|j| j.id).collect();
        assert_eq!(open, ["a", "b"]);

        let mut done = s.job_get("a").unwrap().unwrap().claimed("laptop", 1, t0);
        done.state = JobState::Done {
            runner: "laptop".into(),
            result: JobResult {
                summary: "Fixed".into(),
                ..Default::default()
            },
            finished_at: t0,
        };
        s.job_put(&done, t0).unwrap();
        assert_eq!(s.job_get("a").unwrap().unwrap(), done);
        let open: Vec<_> = s.jobs_open().unwrap().into_iter().map(|j| j.id).collect();
        assert_eq!(open, ["b"]);
        assert_eq!(s.jobs_recent(10).unwrap()[0].id, "b");
    }

    #[test]
    fn runners_and_cloud_connections_are_kept() {
        let s = Store::in_memory().unwrap();
        let now = Utc::now();
        s.runner_put(&RunnerCaps {
            id: "laptop".into(),
            kind: RunnerKind::Computer,
            harnesses: vec!["claude_code".into()],
            seen_at: Some(now),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(s.runners().unwrap()[0].harnesses, ["claude_code"]);

        let settings = json!({ "repos": "acme/api" });
        s.cloud_connection_set("github", settings.as_object(), now)
            .unwrap();
        let c = s.cloud_connections().unwrap();
        assert_eq!(
            (c[0].id.as_str(), c[0].settings["repos"].as_str()),
            ("github", Some("acme/api"))
        );
        s.cloud_connection_set("github", None, now).unwrap();
        assert!(s.cloud_connections().unwrap().is_empty());
    }
}
