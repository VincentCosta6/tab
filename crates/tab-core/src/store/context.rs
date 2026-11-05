//! Context attached to items, for the chat view and agent prompts.

use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, Row, params};

use super::items::get_item;
use super::{Store, parse_ts, ts};
use crate::model::{ContextEntry, ContextSection, ItemContext, NewContext};

const COLS: &str = "id, item_id, section, pinned, ref_item_id, url, title, summary, added_by, created_at";

impl Store {
    fn row_to_context(&self, conn: &rusqlite::Connection, r: &Row<'_>) -> rusqlite::Result<ContextEntry> {
        let ref_id: Option<String> = r.get(4)?;
        let ref_item = ref_id.and_then(|id| get_item(conn, &id).ok().flatten()).map(Box::new);
        Ok(ContextEntry {
            id: r.get(0)?,
            item_id: r.get(1)?,
            section: ContextSection::parse(&r.get::<_, String>(2)?),
            pinned: r.get::<_, i64>(3)? != 0,
            ref_item,
            url: r.get(5)?,
            title: r.get(6)?,
            summary: r.get(7)?,
            added_by: r.get(8)?,
            created_at: parse_ts(&r.get::<_, String>(9)?)?,
        })
    }

    /// Adds context, or returns the existing entry for the same item or URL.
    /// Re-adding something already in `context` never demotes it to `found`.
    pub fn context_add(&self, item_id: &str, c: &NewContext, now: DateTime<Utc>) -> anyhow::Result<ContextEntry> {
        let id = {
            let conn = self.conn();
            let existing: Option<String> = conn
                .query_row(
                    "SELECT id FROM item_context WHERE item_id = ?1
                     AND ((?2 IS NOT NULL AND ref_item_id = ?2) OR (?3 IS NOT NULL AND url = ?3))",
                    params![item_id, c.ref_item_id, c.url],
                    |r| r.get(0),
                )
                .optional()?;
            match existing {
                Some(id) => {
                    if c.section == ContextSection::Context {
                        conn.execute("UPDATE item_context SET section = 'context' WHERE id = ?1", [&id])?;
                    }
                    id
                }
                None => {
                    let id = uuid::Uuid::new_v4().to_string();
                    conn.execute(
                        "INSERT INTO item_context (id, item_id, section, ref_item_id, url, title, summary, added_by, created_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                        params![
                            id, item_id, c.section.as_str(), c.ref_item_id, c.url, c.title, c.summary, c.added_by,
                            ts(now)
                        ],
                    )?;
                    id
                }
            }
        };
        Ok(self.context_get(&id)?.expect("just written"))
    }

    pub fn context_get(&self, id: &str) -> anyhow::Result<Option<ContextEntry>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!("SELECT {COLS} FROM item_context WHERE id = ?1"))?;
        let mut rows = stmt.query([id])?;
        Ok(match rows.next()? {
            Some(r) => Some(self.row_to_context(&conn, r)?),
            None => None,
        })
    }

    pub fn context_update(&self, id: &str, section: Option<ContextSection>, pinned: Option<bool>) -> anyhow::Result<Option<ContextEntry>> {
        {
            let conn = self.conn();
            if let Some(s) = section {
                conn.execute("UPDATE item_context SET section = ?2 WHERE id = ?1", params![id, s.as_str()])?;
            }
            if let Some(p) = pinned {
                conn.execute("UPDATE item_context SET pinned = ?2 WHERE id = ?1", params![id, p as i64])?;
            }
        }
        self.context_get(id)
    }

    pub fn context_remove(&self, id: &str) -> anyhow::Result<()> {
        self.conn().execute("DELETE FROM item_context WHERE id = ?1", [id])?;
        Ok(())
    }

    /// The item, its context entries (pinned first), and graph suggestions not yet attached.
    pub fn item_context(&self, item_id: &str) -> anyhow::Result<Option<ItemContext>> {
        let Some(item) = self.get(item_id)? else { return Ok(None) };
        let entries: Vec<ContextEntry> = {
            let conn = self.conn();
            let mut stmt = conn.prepare(&format!(
                "SELECT {COLS} FROM item_context WHERE item_id = ?1 ORDER BY section, pinned DESC, created_at"
            ))?;
            let mut rows = stmt.query([item_id])?;
            let mut out = Vec::new();
            while let Some(r) = rows.next()? {
                out.push(self.row_to_context(&conn, r)?);
            }
            out
        };
        let attached: std::collections::HashSet<String> =
            entries.iter().filter_map(|e| e.ref_item.as_ref().map(|i| i.id.clone())).collect();
        let related = self.related_items(item_id, 12)?.into_iter().filter(|i| !attached.contains(&i.id)).take(6).collect();
        Ok(Some(ItemContext { item, entries, related }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{EntityRef, IncomingEvent};
    use crate::store::Upsert;

    fn add(s: &Store, id: &str) -> String {
        let e = IncomingEvent {
            source: "t".into(),
            external_id: id.into(),
            kind: "t.k".into(),
            title: id.into(),
            occurred_at: Utc::now(),
            entities: vec![EntityRef::new("repo", "github:acme/x", "acme/x", "in")],
            ..Default::default()
        };
        match s.upsert(&e, Utc::now()).unwrap() {
            Upsert::Inserted(i) => i.id,
            _ => unreachable!(),
        }
    }

    #[test]
    fn found_promotes_to_context_and_related_excludes_attached() {
        let s = Store::in_memory().unwrap();
        let (task, a, b) = (add(&s, "task"), add(&s, "a"), add(&s, "b"));
        let found = s
            .context_add(&task, &NewContext {
                section: ContextSection::Found,
                ref_item_id: Some(a.clone()),
                url: None,
                title: "a".into(),
                summary: None,
                added_by: "agent".into(),
            }, Utc::now())
            .unwrap();
        assert_eq!(found.ref_item.as_ref().unwrap().id, a);

        let ctx = s.item_context(&task).unwrap().unwrap();
        assert_eq!(ctx.entries.len(), 1);
        assert_eq!(ctx.related.iter().map(|i| i.id.clone()).collect::<Vec<_>>(), [b.clone()]);

        let promoted = s.context_update(&found.id, Some(ContextSection::Context), Some(true)).unwrap().unwrap();
        assert_eq!((promoted.section, promoted.pinned), (ContextSection::Context, true));

        // Re-adding the same link as found doesn't demote it or duplicate it.
        let again = s
            .context_add(&task, &NewContext {
                section: ContextSection::Found,
                ref_item_id: Some(a),
                url: None,
                title: "a".into(),
                summary: None,
                added_by: "agent".into(),
            }, Utc::now())
            .unwrap();
        assert_eq!((again.id, again.section), (found.id, ContextSection::Context));
    }
}
