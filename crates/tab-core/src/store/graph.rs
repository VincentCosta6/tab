//! Knowledge graph queries. Items link to entities; entities that appear on
//! the same item get a weighted `co_occurs` edge. That's enough for "what's
//! related to this" today, and the MCP server builds on the same tables.

use chrono::{DateTime, Utc};
use rusqlite::{Transaction, params};

use super::items::get_item;
use super::{Store, fts_phrase, ts};
use crate::model::{Entity, EntityRef, QueueItem};

pub(crate) fn link_item(
    tx: &Transaction<'_>,
    item_id: &str,
    refs: &[EntityRef],
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    // Project membership is set by the person, not the source: keep it.
    tx.execute(
        "DELETE FROM item_entities WHERE item_id = ?1
           AND entity_id NOT IN (SELECT id FROM entities WHERE kind = 'project')",
        [item_id],
    )?;
    let mut ids = Vec::with_capacity(refs.len());
    for r in refs {
        let id: i64 = tx.query_row(
            "INSERT INTO entities (kind, key, label, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4)
             ON CONFLICT (kind, key) DO UPDATE SET label = excluded.label, updated_at = excluded.updated_at
             RETURNING id",
            params![r.kind, r.key, r.label, ts(now)],
            |row| row.get(0),
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO item_entities (item_id, entity_id, rel) VALUES (?1, ?2, ?3)",
            params![item_id, id, r.rel],
        )?;
        ids.push(id);
    }
    ids.sort_unstable();
    ids.dedup();
    for (i, a) in ids.iter().enumerate() {
        for b in &ids[i + 1..] {
            tx.execute(
                "INSERT INTO edges (src, dst, rel, weight, updated_at) VALUES (?1, ?2, 'co_occurs', 1, ?3)
                 ON CONFLICT (src, dst, rel) DO UPDATE SET weight = weight + 1, updated_at = excluded.updated_at",
                params![a, b, ts(now)],
            )?;
        }
    }
    Ok(())
}

/// Entities for a GitHub PR or issue URL: its repo and the PR or issue itself,
/// keyed exactly as the GitHub connector keys them so they are the same nodes.
pub(crate) fn github_refs(url: &str) -> Vec<EntityRef> {
    let Some(rest) = url
        .trim()
        .strip_prefix("https://github.com/")
        .or_else(|| url.trim().strip_prefix("http://github.com/"))
    else {
        return Vec::new();
    };
    let parts: Vec<&str> = rest.split(['/', '?', '#']).collect();
    let [owner, repo, kind, number, ..] = parts[..] else {
        return Vec::new();
    };
    let kind = match kind {
        "pull" => "pull_request",
        "issues" => "issue",
        _ => return Vec::new(),
    };
    if owner.is_empty()
        || repo.is_empty()
        || number.is_empty()
        || !number.bytes().all(|b| b.is_ascii_digit())
    {
        return Vec::new();
    }
    let full = format!("{owner}/{repo}");
    vec![
        EntityRef::new("repo", format!("github:{full}"), full.clone(), "in"),
        EntityRef::new(
            kind,
            format!("github:{full}#{number}"),
            format!("{full}#{number}"),
            "about",
        ),
    ]
}

/// Adds entities to an item without touching what it already links to, and
/// joins each new one to the others on the item with `co_occurs` edges.
pub(crate) fn add_refs(
    tx: &Transaction<'_>,
    item_id: &str,
    refs: &[EntityRef],
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    for r in refs {
        let id: i64 = tx.query_row(
            "INSERT INTO entities (kind, key, label, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4)
             ON CONFLICT (kind, key) DO UPDATE SET label = excluded.label, updated_at = excluded.updated_at
             RETURNING id",
            params![r.kind, r.key, r.label, ts(now)],
            |row| row.get(0),
        )?;
        let added = tx.execute(
            "INSERT OR IGNORE INTO item_entities (item_id, entity_id, rel) VALUES (?1, ?2, ?3)",
            params![item_id, id, r.rel],
        )?;
        if added == 0 {
            continue;
        }
        let others: Vec<i64> = tx
            .prepare("SELECT entity_id FROM item_entities WHERE item_id = ?1 AND entity_id != ?2")?
            .query_map(params![item_id, id], |row| row.get(0))?
            .collect::<Result<_, _>>()?;
        for o in others {
            let (a, b) = (id.min(o), id.max(o));
            tx.execute(
                "INSERT INTO edges (src, dst, rel, weight, updated_at) VALUES (?1, ?2, 'co_occurs', 1, ?3)
                 ON CONFLICT (src, dst, rel) DO UPDATE SET weight = weight + 1, updated_at = excluded.updated_at",
                params![a, b, ts(now)],
            )?;
        }
    }
    Ok(())
}

/// Two entity ids and how many items they share.
pub type GraphEdge = (i64, i64, i64);

const ENTITY_COLS: &str = "e.id, e.kind, e.key, e.label, (SELECT COUNT(*) FROM item_entities ie WHERE ie.entity_id = e.id)";

fn row_to_entity(r: &rusqlite::Row<'_>) -> rusqlite::Result<Entity> {
    Ok(Entity {
        id: r.get(0)?,
        kind: r.get(1)?,
        key: r.get(2)?,
        label: r.get(3)?,
        item_count: r.get(4)?,
    })
}

impl Store {
    /// Other items that really overlap with this one, most shared first.
    ///
    /// Being in the same project isn't overlap (the project view covers
    /// that), and neither is an entity on a large share of everything, like
    /// you or a busy repo. What's left counts when it's specific (the same
    /// PR, ticket or thread) or when two or more broad ones line up.
    pub fn related_items(&self, item_id: &str, limit: usize) -> anyhow::Result<Vec<QueueItem>> {
        let conn = self.conn();
        let ids: Vec<String> = conn
            .prepare(
                "SELECT other.item_id, COUNT(DISTINCT other.entity_id) AS shared,
                        SUM(e.kind NOT IN ('person', 'repo', 'channel', 'org', 'team', 'mailbox', 'domain')) AS specific,
                        MAX(i.occurred_at) AS latest
                 FROM item_entities mine
                 JOIN entities e ON e.id = mine.entity_id
                 JOIN item_entities other ON other.entity_id = mine.entity_id AND other.item_id != mine.item_id
                 JOIN items i ON i.id = other.item_id
                 WHERE mine.item_id = ?1
                   AND e.kind != 'project'
                   AND (SELECT COUNT(*) FROM item_entities x WHERE x.entity_id = e.id) <= 40
                 GROUP BY other.item_id
                 HAVING shared >= 2 OR specific >= 1
                 ORDER BY shared DESC, latest DESC, other.item_id
                 LIMIT ?2",
            )?
            .query_map(params![item_id, limit], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            out.extend(get_item(&conn, &id)?);
        }
        Ok(out)
    }

    pub fn search_entities(&self, query: &str, limit: usize) -> anyhow::Result<Vec<Entity>> {
        let conn = self.conn();
        let rows = match fts_phrase(query) {
            Some(phrase) => conn
                .prepare(&format!(
                    "SELECT {ENTITY_COLS} FROM entities_fts JOIN entities e ON e.id = entities_fts.rowid
                     WHERE entities_fts MATCH ?1 ORDER BY bm25(entities_fts), e.id LIMIT ?2"
                ))?
                .query_map(params![phrase, limit], row_to_entity)?
                .collect::<Result<_, _>>()?,
            None => conn
                .prepare(&format!("SELECT {ENTITY_COLS} FROM entities e WHERE e.label LIKE ?1 ORDER BY e.label LIMIT ?2"))?
                .query_map(params![format!("%{}%", query.trim()), limit], row_to_entity)?
                .collect::<Result<_, _>>()?,
        };
        Ok(rows)
    }

    pub fn entity(&self, id: i64) -> anyhow::Result<Option<Entity>> {
        use rusqlite::OptionalExtension;
        let conn = self.conn();
        Ok(conn
            .query_row(
                &format!("SELECT {ENTITY_COLS} FROM entities e WHERE e.id = ?1"),
                [id],
                row_to_entity,
            )
            .optional()?)
    }

    /// Items linked to an entity, most recent first, optionally only since a time.
    pub fn entity_items(
        &self,
        entity_id: i64,
        since: Option<DateTime<Utc>>,
        limit: usize,
    ) -> anyhow::Result<Vec<QueueItem>> {
        let conn = self.conn();
        let ids: Vec<String> = conn
            .prepare(
                "SELECT i.id FROM item_entities ie JOIN items i ON i.id = ie.item_id
                 WHERE ie.entity_id = ?1 AND (?3 IS NULL OR i.occurred_at >= ?3)
                 ORDER BY i.occurred_at DESC LIMIT ?2",
            )?
            .query_map(params![entity_id, limit, since.map(ts)], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            out.extend(get_item(&conn, &id)?);
        }
        Ok(out)
    }

    /// Entities that appear on the same items as this one, optionally of one
    /// kind, with how many items they share. Projects count too, so a repo
    /// leads to the project its work is filed under.
    pub fn co_entities(
        &self,
        entity_id: i64,
        kind: Option<&str>,
        since: Option<DateTime<Utc>>,
        limit: usize,
    ) -> anyhow::Result<Vec<(Entity, i64)>> {
        let conn = self.conn();
        let rows = conn
            .prepare(&format!(
                "SELECT {ENTITY_COLS}, COUNT(DISTINCT mine.item_id) AS shared
                 FROM item_entities mine
                 JOIN item_entities other ON other.item_id = mine.item_id AND other.entity_id != mine.entity_id
                 JOIN entities e ON e.id = other.entity_id
                 WHERE mine.entity_id = ?1 AND (?2 IS NULL OR e.kind = ?2)
                   AND (?4 IS NULL OR mine.item_id IN (SELECT id FROM items WHERE occurred_at >= ?4))
                 GROUP BY e.id ORDER BY shared DESC, e.label LIMIT ?3"
            ))?
            .query_map(params![entity_id, kind, limit, since.map(ts)], |r| Ok((row_to_entity(r)?, r.get(5)?)))?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// A drawable slice of the graph: the `limit` busiest entities (in one
    /// project's items, or everywhere), and how many items each pair shares.
    /// Projects are nodes too, so their work clusters around them.
    pub fn graph_view(
        &self,
        project_id: Option<&str>,
        since: Option<DateTime<Utc>>,
        limit: usize,
    ) -> anyhow::Result<(Vec<Entity>, Vec<GraphEdge>)> {
        let conn = self.conn();
        // The items in scope, as a reusable subquery.
        let scope = "SELECT id FROM items WHERE (?1 IS NULL OR project_id = ?1) AND (?2 IS NULL OR occurred_at >= ?2)";
        let since = since.map(ts);
        let nodes: Vec<Entity> = conn
            .prepare(&format!(
                "SELECT e.id, e.kind, e.key, e.label, COUNT(*) AS n
                 FROM item_entities ie JOIN entities e ON e.id = ie.entity_id
                 WHERE ie.item_id IN ({scope})
                 GROUP BY e.id ORDER BY n DESC, e.label LIMIT ?3"
            ))?
            .query_map(params![project_id, since, limit], row_to_entity)?
            .collect::<Result<_, _>>()?;
        if nodes.is_empty() {
            return Ok((nodes, Vec::new()));
        }
        let ids = nodes
            .iter()
            .map(|e| e.id.to_string())
            .collect::<Vec<_>>()
            .join(",");
        // Ids are integers we just read, so inlining them is safe.
        let edges = conn
            .prepare(&format!(
                "SELECT a.entity_id, b.entity_id, COUNT(*) FROM item_entities a
                 JOIN item_entities b ON b.item_id = a.item_id AND a.entity_id < b.entity_id
                 WHERE a.entity_id IN ({ids}) AND b.entity_id IN ({ids}) AND a.item_id IN ({scope})
                 GROUP BY a.entity_id, b.entity_id"
            ))?
            .query_map(params![project_id, since], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?
            .collect::<Result<_, _>>()?;
        Ok((nodes, edges))
    }

    /// Entities most connected to the given one.
    pub fn neighbors(&self, entity_id: i64, limit: usize) -> anyhow::Result<Vec<(Entity, f64)>> {
        let conn = self.conn();
        let rows = conn
            .prepare(&format!(
                "SELECT {ENTITY_COLS}, n.weight FROM (
                     SELECT dst AS id, weight FROM edges WHERE src = ?1
                     UNION ALL SELECT src AS id, weight FROM edges WHERE dst = ?1
                 ) n JOIN entities e ON e.id = n.id
                 ORDER BY n.weight DESC, e.id LIMIT ?2"
            ))?
            .query_map(params![entity_id, limit], |r| {
                Ok((row_to_entity(r)?, r.get(5)?))
            })?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::IncomingEvent;

    fn ev(id: &str, refs: Vec<EntityRef>) -> IncomingEvent {
        IncomingEvent {
            source: "t".into(),
            external_id: id.into(),
            kind: "t.k".into(),
            title: format!("item {id}"),
            occurred_at: Utc::now(),
            entities: refs,
            ..Default::default()
        }
    }

    #[test]
    fn graph_view_scopes_to_a_project() {
        let s = Store::in_memory().unwrap();
        let now = Utc::now();
        let p = s
            .project_save(None, "Payments", "#7c5cff", "zap", now)
            .unwrap();
        let dana = || EntityRef::new("person", "github:dana", "Dana", "from");
        let pay = || EntityRef::new("repo", "github:acme/payments", "acme/payments", "in");
        let web = || EntityRef::new("repo", "github:acme/web", "acme/web", "in");
        let id = |u| match u {
            crate::store::Upsert::Inserted(i) => i.id,
            _ => unreachable!(),
        };
        let a = id(s.upsert(&ev("a", vec![dana(), pay()]), now).unwrap());
        id(s.upsert(&ev("b", vec![dana(), web()]), now).unwrap());
        s.item_set_project(&a, Some(&p.id), now).unwrap();

        let (nodes, edges) = s.graph_view(None, None, 50).unwrap();
        assert_eq!(nodes.len(), 4, "dana, two repos, the project");
        assert_eq!(nodes[0].label, "Dana");
        assert_eq!(
            edges.len(),
            4,
            "dana-payments, dana-web, and the project with dana and payments"
        );

        let (nodes, _) = s.graph_view(Some(&p.id), None, 50).unwrap();
        let labels: Vec<_> = nodes.iter().map(|n| n.label.as_str()).collect();
        assert!(!labels.contains(&"acme/web"), "{labels:?}");
        assert_eq!(nodes.len(), 3);
    }

    #[test]
    fn related_items_and_neighbors() {
        let s = Store::in_memory().unwrap();
        let dana = || EntityRef::new("person", "github:dana", "Dana", "from");
        let repo = || EntityRef::new("repo", "github:acme/payments", "acme/payments", "in");
        let other = || EntityRef::new("repo", "github:acme/web", "acme/web", "in");
        let id_of = |u| match u {
            crate::store::Upsert::Inserted(i) => i.id,
            _ => unreachable!(),
        };
        let a = id_of(
            s.upsert(&ev("a", vec![dana(), repo()]), Utc::now())
                .unwrap(),
        );
        let b = id_of(
            s.upsert(&ev("b", vec![dana(), repo()]), Utc::now())
                .unwrap(),
        );
        id_of(
            s.upsert(&ev("c", vec![dana(), other()]), Utc::now())
                .unwrap(),
        );
        id_of(s.upsert(&ev("d", vec![]), Utc::now()).unwrap());

        let related: Vec<_> = s
            .related_items(&a, 10)
            .unwrap()
            .into_iter()
            .map(|i| i.id)
            .collect();
        assert_eq!(
            related,
            [b],
            "b shares two entities; c shares only a person, d nothing"
        );

        let found = s.search_entities("payme", 5).unwrap();
        assert_eq!(found[0].key, "github:acme/payments");
        assert_eq!(found[0].item_count, 2);

        let dana_id = s.search_entities("Dana", 1).unwrap()[0].id;
        let n = s.neighbors(dana_id, 5).unwrap();
        assert_eq!(n[0].0.key, "github:acme/payments");
        assert_eq!(n[0].1, 2.0);
    }
}
