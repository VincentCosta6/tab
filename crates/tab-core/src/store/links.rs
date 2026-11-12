//! How tasks relate: follow-ups nest under the task they came from, and a
//! task can have to wait for others to be done first.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use rusqlite::{Connection, params};

use super::{Store, ts};
use crate::model::{LinkKind, QueueItem};

/// Fills in [`QueueItem::parent_id`] and [`QueueItem::blocked_by`].
pub(crate) fn fill(conn: &Connection, items: &mut [QueueItem]) -> anyhow::Result<()> {
    if items.is_empty() {
        return Ok(());
    }
    let one = (items.len() == 1).then(|| items[0].id.clone());
    // The table is small: one task's links, or all of them for a list.
    let mut stmt = conn.prepare_cached(
        "SELECT l.src, l.dst, l.kind FROM item_links l
         JOIN items o ON o.id = CASE l.kind WHEN 'blocks' THEN l.src ELSE l.dst END
         WHERE (?1 IS NULL OR CASE l.kind WHEN 'blocks' THEN l.dst ELSE l.src END = ?1)
           AND (l.kind = 'followup' OR o.state != 'done')
         ORDER BY l.created_at, l.src",
    )?;
    let mut parent: HashMap<String, String> = HashMap::new();
    let mut blocked: HashMap<String, Vec<String>> = HashMap::new();
    let rows = stmt.query_map([one], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    for row in rows {
        let (src, dst, kind) = row?;
        if kind == "blocks" {
            blocked.entry(dst).or_default().push(src);
        } else {
            parent.insert(src, dst);
        }
    }
    for i in items {
        i.parent_id = parent.remove(&i.id);
        i.blocked_by = blocked.remove(&i.id).unwrap_or_default();
    }
    Ok(())
}

/// Whether `to` can be reached from `from` along links of `kind`.
fn reaches(conn: &Connection, from: &str, to: &str, kind: LinkKind) -> anyhow::Result<bool> {
    Ok(conn.query_row(
        "WITH RECURSIVE r(id) AS (
             SELECT ?1 UNION SELECT l.dst FROM item_links l JOIN r ON l.src = r.id WHERE l.kind = ?3
         ) SELECT EXISTS (SELECT 1 FROM r WHERE id = ?2)",
        params![from, to, kind.as_str()],
        |r| r.get(0),
    )?)
}

impl Store {
    /// Links `src` to `dst`. A task has one parent, so a new follow-up link
    /// replaces the old one. Links that would go round in a circle are refused.
    pub fn link_add(
        &self,
        src: &str,
        dst: &str,
        kind: LinkKind,
        now: DateTime<Utc>,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(src != dst, "a task can't be linked to itself");
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        for id in [src, dst] {
            anyhow::ensure!(
                super::items::get_item(&tx, id)?.is_some(),
                "no item with id {id}"
            );
        }
        anyhow::ensure!(
            !reaches(&tx, dst, src, kind)?,
            "that would make a loop: the other task already {} this one",
            match kind {
                LinkKind::Followup => "comes from",
                LinkKind::Blocks => "waits on",
            }
        );
        if kind == LinkKind::Followup {
            tx.execute(
                "DELETE FROM item_links WHERE src = ?1 AND kind = 'followup' AND dst != ?2",
                params![src, dst],
            )?;
        }
        tx.execute(
            "INSERT OR IGNORE INTO item_links (src, dst, kind, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![src, dst, kind.as_str(), ts(now)],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn link_remove(&self, src: &str, dst: &str, kind: LinkKind) -> anyhow::Result<bool> {
        Ok(self.conn().execute(
            "DELETE FROM item_links WHERE src = ?1 AND dst = ?2 AND kind = ?3",
            params![src, dst, kind.as_str()],
        )? > 0)
    }

    /// Follow-ups of `id`, oldest first.
    pub fn followups(&self, id: &str) -> anyhow::Result<Vec<QueueItem>> {
        let conn = self.conn();
        let ids: Vec<String> = conn
            .prepare_cached(
                "SELECT src FROM item_links WHERE dst = ?1 AND kind = 'followup' ORDER BY created_at, src",
            )?
            .query_map([id], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        let mut out = vec![];
        for id in ids {
            out.extend(super::items::get_item(&conn, &id)?);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{IncomingEvent, ItemState};
    use crate::store::Upsert;

    fn add(s: &Store, id: &str) -> String {
        let e = IncomingEvent {
            source: "t".into(),
            external_id: id.into(),
            kind: "t.k".into(),
            title: id.into(),
            occurred_at: Utc::now(),
            ..Default::default()
        };
        match s.upsert(&e, Utc::now()).unwrap() {
            Upsert::Inserted(i) => i.id,
            _ => unreachable!(),
        }
    }

    #[test]
    fn followups_and_blockers_show_on_items() {
        let s = Store::in_memory().unwrap();
        let (hub, p2, p3, p4) = (add(&s, "hub"), add(&s, "p2"), add(&s, "p3"), add(&s, "p4"));
        for p in [&p2, &p3, &p4] {
            s.link_add(p, &hub, LinkKind::Followup, Utc::now()).unwrap();
        }
        s.link_add(&p2, &p3, LinkKind::Blocks, Utc::now()).unwrap();
        s.link_add(&p3, &p4, LinkKind::Blocks, Utc::now()).unwrap();

        let get = |id: &str| s.get(id).unwrap().unwrap();
        assert_eq!(get(&p3).parent_id.as_deref(), Some(hub.as_str()));
        assert_eq!(get(&p3).blocked_by, [p2.as_str()]);
        assert!(get(&hub).parent_id.is_none());
        let listed = s.list(&[ItemState::Open]).unwrap();
        let p4_listed = listed.iter().find(|i| i.id == p4).unwrap();
        assert_eq!(p4_listed.blocked_by, [p3.as_str()]);
        assert_eq!(
            s.followups(&hub)
                .unwrap()
                .iter()
                .map(|i| &i.id)
                .collect::<Vec<_>>(),
            [&p2, &p3, &p4]
        );

        // A blocker that's done no longer holds anything up.
        s.set_state(&p2, ItemState::Done, None, Utc::now()).unwrap();
        assert!(get(&p3).blocked_by.is_empty());
    }

    #[test]
    fn loops_are_refused_and_a_task_has_one_parent() {
        let s = Store::in_memory().unwrap();
        let (a, b, c) = (add(&s, "a"), add(&s, "b"), add(&s, "c"));
        s.link_add(&a, &b, LinkKind::Blocks, Utc::now()).unwrap();
        s.link_add(&b, &c, LinkKind::Blocks, Utc::now()).unwrap();
        assert!(s.link_add(&c, &a, LinkKind::Blocks, Utc::now()).is_err());
        assert!(s.link_add(&a, &a, LinkKind::Blocks, Utc::now()).is_err());

        s.link_add(&c, &a, LinkKind::Followup, Utc::now()).unwrap();
        s.link_add(&c, &b, LinkKind::Followup, Utc::now()).unwrap();
        assert_eq!(
            s.get(&c).unwrap().unwrap().parent_id.as_deref(),
            Some(b.as_str())
        );
        assert!(s.link_add(&b, &c, LinkKind::Followup, Utc::now()).is_err());

        assert!(s.link_remove(&c, &b, LinkKind::Followup).unwrap());
        assert!(s.get(&c).unwrap().unwrap().parent_id.is_none());
    }

    /// The migration nests the follow-ups agents made before links existed:
    /// an agent's task linked both ways with an older one.
    #[test]
    fn migration_nests_existing_agent_followups() {
        let s = Store::in_memory().unwrap();
        let task = |title: &str, created_by: &str| {
            let e = IncomingEvent {
                source: "tab".into(),
                external_id: title.into(),
                kind: "tab.task".into(),
                title: title.into(),
                occurred_at: Utc::now(),
                metadata: serde_json::json!({ "created_by": created_by }),
                ..Default::default()
            };
            match s.upsert(&e, Utc::now()).unwrap() {
                Upsert::Inserted(i) => i.id,
                _ => unreachable!(),
            }
        };
        let parent = task("hub", "user");
        std::thread::sleep(std::time::Duration::from_millis(5));
        let (child, mine) = (task("phase 2", "agent"), task("mine", "user"));
        let both_ways = |a: &str, b: &str| {
            for (on, other) in [(a, b), (b, a)] {
                s.context_add(
                    on,
                    &crate::model::NewContext {
                        section: crate::model::ContextSection::Context,
                        ref_item_id: Some(other.into()),
                        url: None,
                        title: other.into(),
                        summary: None,
                        added_by: "agent".into(),
                    },
                    Utc::now(),
                )
                .unwrap();
            }
        };
        both_ways(&parent, &child);
        both_ways(&parent, &mine);
        let sql = include_str!("migrations/020_item_links.sql");
        let backfill = &sql[sql.find("INSERT").unwrap()..];
        s.conn().execute_batch(backfill).unwrap();
        assert_eq!(
            s.get(&child).unwrap().unwrap().parent_id.as_deref(),
            Some(parent.as_str())
        );
        // Only tasks an agent made, and never the older one under the newer.
        assert!(s.get(&mine).unwrap().unwrap().parent_id.is_none());
        assert!(s.get(&parent).unwrap().unwrap().parent_id.is_none());
    }
}
