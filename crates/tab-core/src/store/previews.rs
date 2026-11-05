//! Cached rich previews.

use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};

use super::{Store, parse_ts, ts};
use crate::model::{Block, QueueItem};

/// Cached blocks, when they were fetched, and whether the item changed since.
pub type CachedPreview = (Vec<Block>, DateTime<Utc>, bool);
/// An item due for a refresh, its last fetch time, and whether it is dirty.
pub type DuePreview = (QueueItem, Option<DateTime<Utc>>, bool);

impl Store {
    pub fn preview_get(&self, item_id: &str) -> anyhow::Result<Option<CachedPreview>> {
        let row: Option<(String, String, bool)> = self
            .conn()
            .query_row(
                "SELECT blocks, fetched_at, dirty FROM previews WHERE item_id = ?1",
                [item_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        Ok(match row {
            Some((b, t, dirty)) => Some((serde_json::from_str(&b)?, parse_ts(&t)?, dirty)),
            None => None,
        })
    }

    pub fn preview_put(
        &self,
        item_id: &str,
        blocks: &[Block],
        now: DateTime<Utc>,
    ) -> anyhow::Result<()> {
        self.conn().execute(
            "INSERT INTO previews (item_id, blocks, fetched_at, dirty) VALUES (?1, ?2, ?3, 0)
             ON CONFLICT(item_id) DO UPDATE SET blocks = excluded.blocks, fetched_at = excluded.fetched_at, dirty = 0",
            params![item_id, serde_json::to_string(blocks)?, ts(now)],
        )?;
        Ok(())
    }

    /// Marks a preview as out of date without dropping it, so the old
    /// content keeps rendering until the refresher replaces it.
    pub fn preview_mark_dirty(&self, item_id: &str) -> anyhow::Result<()> {
        self.conn().execute(
            "UPDATE previews SET dirty = 1 WHERE item_id = ?1",
            [item_id],
        )?;
        Ok(())
    }

    /// Open items whose preview is missing, dirty, or older than `before`,
    /// most important first. The refresher filters by connector TTL.
    pub fn previews_due(
        &self,
        before: DateTime<Utc>,
        limit: usize,
    ) -> anyhow::Result<Vec<DuePreview>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT items.id, previews.fetched_at, COALESCE(previews.dirty, 0) FROM items
             LEFT JOIN previews ON previews.item_id = items.id
             WHERE items.state = 'open'
               AND (previews.item_id IS NULL OR previews.dirty = 1 OR previews.fetched_at < ?1)
             ORDER BY previews.item_id IS NOT NULL, previews.dirty DESC, COALESCE(items.score, 50) DESC
             LIMIT ?2",
        )?;
        let ids: Vec<(String, Option<String>, bool)> = stmt
            .query_map(params![ts(before), limit], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?
            .collect::<Result<_, _>>()?;
        drop(stmt);
        drop(conn);
        let mut out = Vec::new();
        for (id, at, dirty) in ids {
            if let Some(item) = self.get(&id)? {
                out.push((item, at.map(|t| parse_ts(&t)).transpose()?, dirty));
            }
        }
        Ok(out)
    }
}
