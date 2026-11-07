//! Context attached to items, for the chat view and agent prompts.

use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, Row, params};

use super::items::{get_item, item_by_url};
use super::{Store, parse_ts, ts};
use crate::model::{ContextEntry, ContextSection, ItemContext, NewContext};

const COLS: &str =
    "id, item_id, section, pinned, ref_item_id, url, title, summary, added_by, created_at";

/// A link found on the item itself isn't context for it.
fn ref_id_differs(i: &crate::model::QueueItem, r: &Row<'_>) -> bool {
    r.get::<_, String>(1).map(|own| own != i.id).unwrap_or(true)
}

impl Store {
    fn row_to_context(
        &self,
        conn: &rusqlite::Connection,
        r: &Row<'_>,
    ) -> rusqlite::Result<ContextEntry> {
        let ref_id: Option<String> = r.get(4)?;
        let url: Option<String> = r.get(5)?;
        // A link to something in the queue shows as that item.
        let ref_item = match ref_id {
            Some(id) => get_item(conn, &id).ok().flatten(),
            None => url
                .as_deref()
                .and_then(|u| item_by_url(conn, u).ok().flatten()),
        }
        .filter(|i| ref_id_differs(i, r))
        .map(Box::new);
        Ok(ContextEntry {
            id: r.get(0)?,
            item_id: r.get(1)?,
            section: ContextSection::parse(&r.get::<_, String>(2)?),
            pinned: r.get::<_, i64>(3)? != 0,
            ref_item,
            url,
            title: r.get(6)?,
            summary: r.get(7)?,
            added_by: r.get(8)?,
            created_at: parse_ts(&r.get::<_, String>(9)?)?,
        })
    }

    /// Adds context, or returns the existing entry for the same item or URL.
    /// Re-adding something already in `context` never demotes it to `found`.
    pub fn context_add(
        &self,
        item_id: &str,
        c: &NewContext,
        now: DateTime<Utc>,
    ) -> anyhow::Result<ContextEntry> {
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
                        conn.execute(
                            "UPDATE item_context SET section = 'context' WHERE id = ?1",
                            [&id],
                        )?;
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
        // Attached work joins the graph: a PR or issue link ties the task to
        // that PR and its repo, so it shows up in the project's graph.
        if c.section == ContextSection::Context
            && let Some(url) = &c.url
        {
            let refs = super::graph::github_refs(url);
            if !refs.is_empty() {
                let mut conn = self.conn();
                let tx = conn.transaction()?;
                super::graph::add_refs(&tx, item_id, &refs, now)?;
                tx.commit()?;
            }
        }
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

    pub fn context_update(
        &self,
        id: &str,
        section: Option<ContextSection>,
        pinned: Option<bool>,
    ) -> anyhow::Result<Option<ContextEntry>> {
        {
            let conn = self.conn();
            if let Some(s) = section {
                conn.execute(
                    "UPDATE item_context SET section = ?2 WHERE id = ?1",
                    params![id, s.as_str()],
                )?;
            }
            if let Some(p) = pinned {
                conn.execute(
                    "UPDATE item_context SET pinned = ?2 WHERE id = ?1",
                    params![id, p as i64],
                )?;
            }
        }
        self.context_get(id)
    }

    pub fn context_remove(&self, id: &str) -> anyhow::Result<()> {
        self.conn()
            .execute("DELETE FROM item_context WHERE id = ?1", [id])?;
        Ok(())
    }

    /// The item, its context entries (pinned first), and graph suggestions not yet attached.
    pub fn item_context(&self, item_id: &str) -> anyhow::Result<Option<ItemContext>> {
        let Some(item) = self.get(item_id)? else {
            return Ok(None);
        };
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
        let attached: std::collections::HashSet<String> = entries
            .iter()
            .filter_map(|e| e.ref_item.as_ref().map(|i| i.id.clone()))
            .collect();
        let related = self
            .related_items(item_id, 12)?
            .into_iter()
            .filter(|i| !attached.contains(&i.id))
            .take(6)
            .collect();
        Ok(Some(ItemContext {
            item,
            entries,
            related,
        }))
    }

    /// The tasks that have this link in their Context, by URL or because the
    /// link is a queue item they attach. Query, fragment and subpages like a
    /// PR's /files are ignored. Newest first.
    pub fn items_with_context_link(
        &self,
        url: &str,
    ) -> anyhow::Result<Vec<crate::model::QueueItem>> {
        fn bare(u: &str) -> &str {
            u.split(['?', '#']).next().unwrap_or("").trim_end_matches('/')
        }
        let url = bare(url);
        if !url.starts_with("http") {
            return Ok(vec![]);
        }
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT DISTINCT ic.item_id FROM item_context ic
             LEFT JOIN items r ON r.id = ic.ref_item_id
             WHERE ic.section = 'context'
               AND ((substr(ic.url, 1, length(?1)) = ?1
                     AND (length(ic.url) = length(?1) OR substr(ic.url, length(?1) + 1, 1) IN ('/', '#', '?')))
                 OR (substr(r.url, 1, length(?1)) = ?1
                     AND (length(r.url) = length(?1) OR substr(r.url, length(?1) + 1, 1) IN ('/', '#', '?'))))",
        )?;
        let ids: Vec<String> = stmt
            .query_map([url], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        let mut out = Vec::new();
        for id in ids {
            if let Some(i) = get_item(&conn, &id)? {
                // The PR's own queue item isn't a task about it.
                if i.event.url.as_deref().map(bare) != Some(url) {
                    out.push(i);
                }
            }
        }
        out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(out)
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
            entities: vec![
                EntityRef::new("repo", "github:acme/x", "acme/x", "in"),
                EntityRef::new("person", "github:dana", "Dana", "from"),
            ],
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
            .context_add(
                &task,
                &NewContext {
                    section: ContextSection::Found,
                    ref_item_id: Some(a.clone()),
                    url: None,
                    title: "a".into(),
                    summary: None,
                    added_by: "agent".into(),
                },
                Utc::now(),
            )
            .unwrap();
        assert_eq!(found.ref_item.as_ref().unwrap().id, a);

        let ctx = s.item_context(&task).unwrap().unwrap();
        assert_eq!(ctx.entries.len(), 1);
        assert_eq!(
            ctx.related.iter().map(|i| i.id.clone()).collect::<Vec<_>>(),
            [b]
        );

        let promoted = s
            .context_update(&found.id, Some(ContextSection::Context), Some(true))
            .unwrap()
            .unwrap();
        assert_eq!(
            (promoted.section, promoted.pinned),
            (ContextSection::Context, true)
        );

        // Re-adding the same link as found doesn't demote it or duplicate it.
        let again = s
            .context_add(
                &task,
                &NewContext {
                    section: ContextSection::Found,
                    ref_item_id: Some(a),
                    url: None,
                    title: "a".into(),
                    summary: None,
                    added_by: "agent".into(),
                },
                Utc::now(),
            )
            .unwrap();
        assert_eq!(
            (again.id, again.section),
            (found.id, ContextSection::Context)
        );
    }

    #[test]
    fn attaching_a_pr_link_puts_it_in_the_graph_once() {
        let s = Store::in_memory().unwrap();
        let task = add(&s, "task");
        let ctx = |url: &str, section| NewContext {
            section,
            ref_item_id: None,
            url: Some(url.into()),
            title: url.into(),
            summary: None,
            added_by: "agent".into(),
        };
        for url in [
            "https://github.com/acme/tab/pull/6",
            "https://github.com/acme/tab/pull/6/files",
        ] {
            s.context_add(&task, &ctx(url, ContextSection::Context), Utc::now())
                .unwrap();
        }
        // Found, and links that aren't a PR or issue, add nothing.
        s.context_add(
            &task,
            &ctx("https://github.com/acme/tab/pull/7", ContextSection::Found),
            Utc::now(),
        )
        .unwrap();
        s.context_add(
            &task,
            &ctx("https://github.com/acme/tab", ContextSection::Context),
            Utc::now(),
        )
        .unwrap();
        let pr = s.search_entities("acme/tab#6", 5).unwrap();
        assert_eq!(pr.len(), 1);
        assert_eq!((pr[0].kind.as_str(), pr[0].item_count), ("pull_request", 1));
        assert!(s.search_entities("acme/tab#7", 5).unwrap().is_empty());
        let (nodes, edges) = s.graph_view(None, None, 20).unwrap();
        assert!(nodes.iter().any(|e| e.key == "github:acme/tab#6"));
        // The new repo and PR also join the task's existing entities, once each.
        assert!(edges.iter().all(|(_, _, n)| *n == 1));
    }
}
