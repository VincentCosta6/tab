//! Projects group work. An item belongs to at most one; the link is stored on
//! the item (for fast filtering) and as an `in` edge to the project's entity
//! node (`project:<id>`), so the graph sees it too.

use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};

use super::items::get_item;
use super::{Store, parse_ts, ts};
use crate::model::{Entity, Project, ProjectNote, QueueItem};

const PROJECT_COLS: &str = "id, name, color, icon, created_at";
const NOTE_COLS: &str =
    "id, project_id, topic, text, added_by, session_id, sources, created_at, updated_at";

fn note_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ProjectNote> {
    Ok(ProjectNote {
        id: r.get("id")?,
        project_id: r.get("project_id")?,
        topic: r.get("topic")?,
        text: r.get("text")?,
        added_by: r.get("added_by")?,
        session_id: r.get("session_id")?,
        sources: serde_json::from_str(&r.get::<_, String>("sources")?).unwrap_or_default(),
        created_at: parse_ts(&r.get::<_, String>("created_at")?)?,
        updated_at: parse_ts(&r.get::<_, String>("updated_at")?)?,
    })
}

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Project> {
    Ok(Project {
        id: r.get("id")?,
        name: r.get("name")?,
        color: r.get("color")?,
        icon: r.get("icon")?,
        created_at: parse_ts(&r.get::<_, String>("created_at")?)?,
    })
}

impl Store {
    pub fn projects(&self) -> anyhow::Result<Vec<Project>> {
        let conn = self.conn();
        let mut st = conn.prepare(&format!(
            "SELECT {PROJECT_COLS} FROM projects ORDER BY name COLLATE NOCASE"
        ))?;
        let out = st.query_map([], row)?.collect::<Result<Vec<_>, _>>()?;
        Ok(out)
    }

    /// Creates a project (no id) or renames / recolors one.
    pub fn project_save(
        &self,
        id: Option<&str>,
        name: &str,
        color: &str,
        icon: &str,
        now: DateTime<Utc>,
    ) -> anyhow::Result<Project> {
        let name = name.trim();
        anyhow::ensure!(!name.is_empty(), "a project needs a name");
        let id = id
            .map(str::to_string)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let conn = self.conn();
        conn.execute(
            "INSERT INTO projects (id, name, color, icon, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?5)
             ON CONFLICT (id) DO UPDATE SET name = excluded.name, color = excluded.color,
                 icon = excluded.icon, updated_at = excluded.updated_at",
            params![id, name, color, icon, ts(now)],
        )?;
        // The project's graph node, named like the project, so agents can
        // find it by name before anything is in it.
        conn.execute(
            "INSERT INTO entities (kind, key, label, created_at, updated_at) VALUES ('project', ?1, ?2, ?3, ?3)
             ON CONFLICT (kind, key) DO UPDATE SET label = excluded.label, updated_at = excluded.updated_at",
            params![format!("project:{id}"), name, ts(now)],
        )?;
        Ok(conn.query_row(
            &format!("SELECT {PROJECT_COLS} FROM projects WHERE id = ?1"),
            [&id],
            row,
        )?)
    }

    /// Deletes a project. Its items stay, without a project.
    pub fn project_delete(&self, id: &str) -> anyhow::Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute(
            "UPDATE items SET project_id = NULL WHERE project_id = ?1",
            [id],
        )?;
        let key = format!("project:{id}");
        tx.execute(
            "DELETE FROM item_entities WHERE entity_id IN (SELECT id FROM entities WHERE kind = 'project' AND key = ?1)",
            [&key],
        )?;
        tx.execute(
            "DELETE FROM entities WHERE kind = 'project' AND key = ?1",
            [&key],
        )?;
        tx.execute("DELETE FROM project_notes WHERE project_id = ?1", [id])?;
        tx.execute("DELETE FROM projects WHERE id = ?1", [id])?;
        tx.commit()?;
        Ok(())
    }

    /// Puts an item in a project, or takes it out (`None`).
    /// Files `attached` under `owner`'s project when it has none of its own,
    /// for things attached as context to a chat. Returns the moved item.
    pub fn adopt_project(
        &self,
        owner: &str,
        attached: &str,
        now: DateTime<Utc>,
    ) -> anyhow::Result<Option<QueueItem>> {
        let (Some(o), Some(a)) = (self.get(owner)?, self.get(attached)?) else {
            return Ok(None);
        };
        match (o.project_id, a.project_id) {
            (Some(p), None) if owner != attached => self.item_set_project(attached, Some(&p), now),
            _ => Ok(None),
        }
    }

    pub fn item_set_project(
        &self,
        item_id: &str,
        project_id: Option<&str>,
        now: DateTime<Utc>,
    ) -> anyhow::Result<Option<QueueItem>> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let project: Option<(String, String)> = match project_id {
            Some(p) => Some(
                tx.query_row("SELECT id, name FROM projects WHERE id = ?1", [p], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .optional()?
                .ok_or_else(|| anyhow::anyhow!("no project {p}"))?,
            ),
            None => None,
        };
        tx.execute(
            "UPDATE items SET project_id = ?2, updated_at = ?3 WHERE id = ?1",
            params![item_id, project.as_ref().map(|p| &p.0), ts(now)],
        )?;
        tx.execute(
            "DELETE FROM item_entities WHERE item_id = ?1
               AND entity_id IN (SELECT id FROM entities WHERE kind = 'project')",
            [item_id],
        )?;
        if let Some((id, name)) = project {
            let entity: i64 = tx.query_row(
                "INSERT INTO entities (kind, key, label, created_at, updated_at) VALUES ('project', ?1, ?2, ?3, ?3)
                 ON CONFLICT (kind, key) DO UPDATE SET label = excluded.label, updated_at = excluded.updated_at
                 RETURNING id",
                params![format!("project:{id}"), name, ts(now)],
                |r| r.get(0),
            )?;
            tx.execute(
                "INSERT OR IGNORE INTO item_entities (item_id, entity_id, rel) VALUES (?1, ?2, 'in')",
                params![item_id, entity],
            )?;
        }
        tx.commit()?;
        get_item(&conn, item_id)
    }

    /// A project by id, or by name (any case).
    pub fn project_find(&self, id_or_name: &str) -> anyhow::Result<Option<Project>> {
        let conn = self.conn();
        Ok(conn
            .query_row(
                &format!(
                    "SELECT {PROJECT_COLS} FROM projects WHERE id = ?1 OR name = ?1 COLLATE NOCASE
                     ORDER BY id = ?1 DESC LIMIT 1"
                ),
                [id_or_name.trim()],
                row,
            )
            .optional()?)
    }

    /// The project's most recently active items, open ones first.
    pub fn project_items(
        &self,
        project_id: &str,
        since: Option<DateTime<Utc>>,
        limit: usize,
    ) -> anyhow::Result<Vec<QueueItem>> {
        let conn = self.conn();
        let ids: Vec<String> = conn
            .prepare(
                "SELECT id FROM items WHERE project_id = ?1 AND (?3 IS NULL OR occurred_at >= ?3)
                 ORDER BY state = 'done', occurred_at DESC LIMIT ?2",
            )?
            .query_map(params![project_id, limit, since.map(ts)], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            out.extend(get_item(&conn, &id)?);
        }
        Ok(out)
    }

    /// What the project's items are about (repos, people, tickets), most
    /// common first, with how many of its items mention each.
    pub fn project_entities(
        &self,
        project_id: &str,
        since: Option<DateTime<Utc>>,
        limit: usize,
    ) -> anyhow::Result<Vec<(Entity, i64)>> {
        let conn = self.conn();
        let rows = conn
            .prepare(
                "SELECT e.id, e.kind, e.key, e.label,
                        (SELECT COUNT(*) FROM item_entities x WHERE x.entity_id = e.id), COUNT(*) AS n
                 FROM items i JOIN item_entities ie ON ie.item_id = i.id JOIN entities e ON e.id = ie.entity_id
                 WHERE i.project_id = ?1 AND e.kind != 'project' AND (?3 IS NULL OR i.occurred_at >= ?3)
                 GROUP BY e.id ORDER BY n DESC, e.label LIMIT ?2",
            )?
            .query_map(params![project_id, limit, since.map(ts)], |r| {
                Ok((
                    Entity { id: r.get(0)?, kind: r.get(1)?, key: r.get(2)?, label: r.get(3)?, item_count: r.get(4)? },
                    r.get(5)?,
                ))
            })?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    pub fn note_get(&self, id: &str) -> anyhow::Result<Option<ProjectNote>> {
        let conn = self.conn();
        Ok(conn
            .query_row(
                &format!("SELECT {NOTE_COLS} FROM project_notes WHERE id = ?1"),
                [id],
                note_row,
            )
            .optional()?)
    }

    pub fn project_notes(&self, project_id: &str) -> anyhow::Result<Vec<ProjectNote>> {
        let conn = self.conn();
        let out = conn
            .prepare(&format!(
                "SELECT {NOTE_COLS} FROM project_notes WHERE project_id = ?1 ORDER BY updated_at DESC"
            ))?
            .query_map([project_id], note_row)?
            .collect::<Result<_, _>>()?;
        Ok(out)
    }

    /// Writes a note. A note with the same topic is replaced, so a project
    /// keeps one current answer per topic instead of a pile of them.
    #[allow(clippy::too_many_arguments)]
    pub fn note_save(
        &self,
        project_id: &str,
        topic: &str,
        text: &str,
        added_by: &str,
        session_id: Option<&str>,
        sources: &[String],
        now: DateTime<Utc>,
    ) -> anyhow::Result<ProjectNote> {
        let (topic, text) = (topic.trim(), text.trim());
        anyhow::ensure!(
            !topic.is_empty() && !text.is_empty(),
            "a note needs a topic and text"
        );
        let conn = self.conn();
        Ok(conn.query_row(
            &format!(
                "INSERT INTO project_notes (id, project_id, topic, text, added_by, session_id, sources, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)
                 ON CONFLICT (project_id, topic) DO UPDATE SET text = excluded.text, added_by = excluded.added_by,
                     session_id = excluded.session_id, sources = excluded.sources, updated_at = excluded.updated_at
                 RETURNING {NOTE_COLS}"
            ),
            params![
                uuid::Uuid::new_v4().to_string(),
                project_id,
                topic,
                text,
                added_by,
                session_id,
                serde_json::to_string(sources)?,
                ts(now)
            ],
            note_row,
        )?)
    }

    /// The person editing a note's text. It becomes theirs.
    pub fn note_edit(
        &self,
        id: &str,
        text: &str,
        now: DateTime<Utc>,
    ) -> anyhow::Result<Option<ProjectNote>> {
        let text = text.trim();
        anyhow::ensure!(!text.is_empty(), "a note needs text");
        let conn = self.conn();
        Ok(conn
            .query_row(
                &format!(
                    "UPDATE project_notes SET text = ?2, added_by = 'user', updated_at = ?3 WHERE id = ?1
                     RETURNING {NOTE_COLS}"
                ),
                params![id, text, ts(now)],
                note_row,
            )
            .optional()?)
    }

    /// Deletes a note. Returns its project.
    pub fn note_delete(&self, id: &str) -> anyhow::Result<Option<String>> {
        let conn = self.conn();
        Ok(conn
            .query_row(
                "DELETE FROM project_notes WHERE id = ?1 RETURNING project_id",
                [id],
                |r| r.get(0),
            )
            .optional()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::IncomingEvent;
    use crate::store::Upsert;

    #[test]
    fn attached_items_adopt_the_chats_project_only_when_unfiled() {
        let s = Store::in_memory().unwrap();
        let now = Utc::now();
        let a = s.project_save(None, "A", "#7c5cff", "folder", now).unwrap();
        let b = s.project_save(None, "B", "#7c5cff", "folder", now).unwrap();
        let add = |id: &str| {
            let e = IncomingEvent {
                source: "tab".into(),
                external_id: id.into(),
                kind: "tab.task".into(),
                title: id.into(),
                occurred_at: now,
                ..Default::default()
            };
            match s.upsert(&e, now).unwrap() {
                Upsert::Inserted(i) => i.id,
                _ => panic!(),
            }
        };
        let (chat, pr, other) = (add("chat"), add("pr"), add("other"));
        s.item_set_project(&chat, Some(&a.id), now).unwrap();
        s.item_set_project(&other, Some(&b.id), now).unwrap();
        let moved = s.adopt_project(&chat, &pr, now).unwrap().unwrap();
        assert_eq!(moved.project_id.as_deref(), Some(a.id.as_str()));
        // Already filed elsewhere: left alone.
        assert!(s.adopt_project(&chat, &other, now).unwrap().is_none());
        assert_eq!(
            s.get(&other).unwrap().unwrap().project_id.as_deref(),
            Some(b.id.as_str())
        );
    }

    #[test]
    fn items_join_and_leave_projects() {
        let s = Store::in_memory().unwrap();
        let now = Utc::now();
        let p = s
            .project_save(None, "Payments", "#7c5cff", "folder", now)
            .unwrap();
        let e = IncomingEvent {
            source: "tab".into(),
            external_id: "1".into(),
            kind: "tab.task".into(),
            title: "Fix checkout".into(),
            occurred_at: now,
            ..Default::default()
        };
        let Upsert::Inserted(item) = s.upsert(&e, now).unwrap() else {
            panic!()
        };
        let moved = s
            .item_set_project(&item.id, Some(&p.id), now)
            .unwrap()
            .unwrap();
        assert_eq!(moved.project_id.as_deref(), Some(p.id.as_str()));
        // The project is a graph node, and it survives the item being re-synced.
        let mut changed = e.clone();
        changed.title = "Fix checkout (again)".into();
        s.upsert(&changed, now).unwrap();
        assert_eq!(
            s.get(&item.id).unwrap().unwrap().project_id.as_deref(),
            Some(p.id.as_str())
        );
        assert_eq!(s.search_entities("Payments", 5).unwrap().len(), 1);

        // Notes: one per topic, the latest wins.
        s.note_save(
            &p.id,
            "deploys",
            "Through staging.",
            "agent",
            None,
            std::slice::from_ref(&item.id),
            now,
        )
        .unwrap();
        let n = s
            .note_save(
                &p.id,
                "deploys",
                "Through staging, then prod on Fridays.",
                "agent",
                None,
                &[],
                now,
            )
            .unwrap();
        assert_eq!(s.project_notes(&p.id).unwrap().len(), 1);
        assert_eq!(
            s.note_edit(&n.id, "Staging only.", now)
                .unwrap()
                .unwrap()
                .added_by,
            "user"
        );
        assert_eq!(s.project_items(&p.id, None, 5).unwrap().len(), 1);
        assert_eq!(s.project_find("payments").unwrap().unwrap().id, p.id);

        s.project_save(Some(&p.id), "Billing", "#00aa88", "card", now)
            .unwrap();
        assert_eq!(s.projects().unwrap()[0].name, "Billing");
        s.project_delete(&p.id).unwrap();
        assert!(s.get(&item.id).unwrap().unwrap().project_id.is_none());
        assert!(s.projects().unwrap().is_empty());
        assert!(s.project_notes(&p.id).unwrap().is_empty());
    }
}
