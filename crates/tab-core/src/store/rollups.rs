//! Daily workflow rollups (see [`tab_domain::workflow::rollup`]), in the
//! synced `workflow_rollups` table: one row per workflow, version and UTC
//! day, the body as JSON.

use chrono::NaiveDate;
use rusqlite::params;
use tab_domain::workflow::rollup::{Plan, Rollup, RollupKey};

use super::Store;

fn day_text(d: NaiveDate) -> String {
    d.format("%Y-%m-%d").to_string()
}

impl Store {
    /// Rollups for UTC days `from <= day < to`; with no workflow, every
    /// workflow's. Rows that don't parse are skipped.
    pub fn workflow_rollups(
        &self,
        workflow_id: Option<&str>,
        from: NaiveDate,
        to: NaiveDate,
    ) -> anyhow::Result<Vec<Rollup>> {
        Ok(self
            .workflow_rollups_stored(workflow_id, from, to)?
            .into_iter()
            .filter_map(|(key, body)| {
                Some(Rollup {
                    workflow_id: key.workflow_id,
                    version: key.version,
                    day: key.day,
                    body: serde_json::from_str(&body).ok()?,
                })
            })
            .collect())
    }

    /// Rollups as stored, the body as its text.
    pub fn workflow_rollups_stored(
        &self,
        workflow_id: Option<&str>,
        from: NaiveDate,
        to: NaiveDate,
    ) -> anyhow::Result<Vec<(RollupKey, String)>> {
        let conn = self.conn();
        let mut st = conn.prepare(
            "SELECT workflow_id, version, day, body FROM workflow_rollups
             WHERE (?1 IS NULL OR workflow_id = ?1) AND day >= ?2 AND day < ?3
             ORDER BY day, workflow_id, version",
        )?;
        let rows = st.query_map(params![workflow_id, day_text(from), day_text(to)], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (workflow_id, version, day, body) = row?;
            let Ok(day) = NaiveDate::parse_from_str(&day, "%Y-%m-%d") else {
                continue;
            };
            out.push((
                RollupKey {
                    workflow_id,
                    version: version as u32,
                    day,
                },
                body,
            ));
        }
        Ok(out)
    }

    /// Writes a [`Plan`] in one transaction.
    pub fn workflow_rollups_apply(&self, plan: &Plan) -> anyhow::Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        for r in &plan.put {
            tx.execute(
                "INSERT INTO workflow_rollups (workflow_id, version, day, body)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (workflow_id, version, day) DO UPDATE SET body = excluded.body
                 WHERE body IS NOT excluded.body",
                params![r.workflow_id, r.version, day_text(r.day), r.body_json()],
            )?;
        }
        for k in &plan.delete {
            tx.execute(
                "DELETE FROM workflow_rollups WHERE workflow_id = ?1 AND version = ?2 AND day = ?3",
                params![k.workflow_id, k.version, day_text(k.day)],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Deletes rollups of days before `before`.
    pub fn workflow_rollups_prune(&self, before: NaiveDate) -> anyhow::Result<usize> {
        Ok(self.conn().execute(
            "DELETE FROM workflow_rollups WHERE day < ?1",
            [day_text(before)],
        )?)
    }
}
