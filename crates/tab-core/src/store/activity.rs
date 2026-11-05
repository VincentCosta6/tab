//! Item timelines.

use chrono::{DateTime, Utc};
use rusqlite::params;

use super::{Store, parse_ts, ts};
use crate::model::Activity;

impl Store {
    pub fn activity_add(
        &self,
        item_id: &str,
        at: DateTime<Utc>,
        kind: &str,
        title: &str,
        by: Option<&str>,
        detail: serde_json::Value,
    ) -> anyhow::Result<()> {
        self.conn().execute(
            "INSERT INTO item_activity (item_id, at, kind, title, by, detail) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![item_id, ts(at), kind, title, by, detail.to_string()],
        )?;
        Ok(())
    }

    /// Everything that happened to an item, oldest first, together with
    /// what happened to the items linked into its context (a PR an agent
    /// raised shows its merge here) and when each was linked.
    pub fn activity_for(&self, item_id: &str, limit: usize) -> anyhow::Result<Vec<Activity>> {
        let conn = self.conn();
        let mut st = conn.prepare(
            "WITH linked AS (
                 SELECT ref_item_id AS id FROM item_context
                  WHERE item_id = ?1 AND ref_item_id IS NOT NULL AND ref_item_id != ?1
                 UNION
                 SELECT i.id FROM item_context c JOIN items i ON i.url = c.url
                  WHERE c.item_id = ?1 AND c.ref_item_id IS NULL AND i.id != ?1
             )
             SELECT a.id, a.item_id, a.at, a.kind, a.title, a.by, a.detail, NULL
               FROM item_activity a WHERE a.item_id = ?1
             UNION ALL
             SELECT a.id, a.item_id, a.at, a.kind, a.title, a.by, a.detail, i.title
               FROM item_activity a JOIN linked l ON l.id = a.item_id JOIN items i ON i.id = a.item_id
              WHERE a.kind IN ('arrived', 'done', 'reopened', 'finished_upstream', 'workflow_run', 'session_started')
             UNION ALL
             SELECT -rowid, c.item_id, c.created_at, 'linked', c.title, c.added_by,
                    json_object('section', c.section, 'url', c.url, 'ref_item_id', c.ref_item_id), NULL
               FROM item_context c WHERE c.item_id = ?1
             ORDER BY 3, 1
             LIMIT ?2",
        )?;
        let rows = st.query_map(params![item_id, limit as i64], |r| {
            Ok(Activity {
                id: r.get(0)?,
                item_id: r.get(1)?,
                at: parse_ts(&r.get::<_, String>(2)?)?,
                kind: r.get(3)?,
                title: r.get(4)?,
                by: r.get(5)?,
                detail: serde_json::from_str(&r.get::<_, String>(6)?).unwrap_or_default(),
                via: r.get(7)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Drops timelines of items that are gone.
    pub fn activity_prune(&self) -> anyhow::Result<usize> {
        Ok(self.conn().execute(
            "DELETE FROM item_activity WHERE item_id NOT IN (SELECT id FROM items)",
            [],
        )?)
    }
}
