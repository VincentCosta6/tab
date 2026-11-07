//! Agent sessions and their transcripts.

use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, Row, params};

use super::{Store, parse_ts, ts};
use crate::model::{AgentKind, Session, SessionMessage, SessionStatus};

const COLS: &str =
    "id, item_id, agent, agent_session_id, title, status, cwd, created_at, updated_at";

fn row_to_session(r: &Row<'_>) -> rusqlite::Result<Session> {
    Ok(Session {
        id: r.get(0)?,
        item_id: r.get(1)?,
        agent: AgentKind::parse(&r.get::<_, String>(2)?),
        agent_session_id: r.get(3)?,
        title: r.get(4)?,
        status: SessionStatus::parse(&r.get::<_, String>(5)?),
        cwd: r.get(6)?,
        created_at: parse_ts(&r.get::<_, String>(7)?)?,
        updated_at: parse_ts(&r.get::<_, String>(8)?)?,
    })
}

impl Store {
    pub fn session_create(
        &self,
        item_id: Option<&str>,
        agent: AgentKind,
        title: &str,
        cwd: &str,
        now: DateTime<Utc>,
    ) -> anyhow::Result<Session> {
        let id = uuid::Uuid::new_v4().to_string();
        self.conn().execute(
            "INSERT INTO sessions (id, item_id, agent, title, status, cwd, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, 'idle', ?5, ?6, ?6)",
            params![id, item_id, agent.as_str(), title, cwd, ts(now)],
        )?;
        Ok(self.session_get(&id)?.expect("just written"))
    }

    pub fn session_get(&self, id: &str) -> anyhow::Result<Option<Session>> {
        Ok(self
            .conn()
            .query_row(
                &format!("SELECT {COLS} FROM sessions WHERE id = ?1"),
                [id],
                row_to_session,
            )
            .optional()?)
    }

    /// The session to reuse for an item: the most recently active one.
    pub fn session_for_item(&self, item_id: &str) -> anyhow::Result<Option<Session>> {
        Ok(self
            .conn()
            .query_row(
                &format!("SELECT {COLS} FROM sessions WHERE item_id = ?1 ORDER BY updated_at DESC LIMIT 1"),
                [item_id],
                row_to_session,
            )
            .optional()?)
    }

    /// The newest session with this key (e.g. `review:owner/name#12`).
    pub fn session_for_key(&self, key: &str) -> anyhow::Result<Option<Session>> {
        Ok(self
            .conn()
            .query_row(
                &format!(
                    "SELECT {COLS} FROM sessions WHERE key = ?1 ORDER BY updated_at DESC LIMIT 1"
                ),
                [key],
                row_to_session,
            )
            .optional()?)
    }

    pub fn session_create_keyed(
        &self,
        key: &str,
        agent: AgentKind,
        title: &str,
        cwd: &str,
        now: DateTime<Utc>,
    ) -> anyhow::Result<Session> {
        let s = self.session_create(None, agent, title, cwd, now)?;
        self.conn().execute(
            "UPDATE sessions SET key = ?2 WHERE id = ?1",
            params![s.id, key],
        )?;
        Ok(s)
    }

    pub fn session_key(&self, id: &str) -> anyhow::Result<Option<String>> {
        Ok(self
            .conn()
            .query_row("SELECT key FROM sessions WHERE id = ?1", [id], |r| {
                r.get::<_, Option<String>>(0)
            })
            .optional()?
            .flatten())
    }

    /// Moves a session to a new key, e.g. when the draft it was about gets saved.
    pub fn session_set_key(&self, id: &str, key: &str) -> anyhow::Result<()> {
        self.conn().execute(
            "UPDATE sessions SET key = ?2 WHERE id = ?1",
            params![id, key],
        )?;
        Ok(())
    }

    /// What the agent should know about the session's subject, refreshed each turn.
    pub fn session_set_brief(&self, id: &str, brief: &str) -> anyhow::Result<()> {
        self.conn().execute(
            "UPDATE sessions SET brief = ?2 WHERE id = ?1",
            params![id, brief],
        )?;
        Ok(())
    }

    pub fn session_brief(&self, id: &str) -> anyhow::Result<Option<String>> {
        Ok(self
            .conn()
            .query_row("SELECT brief FROM sessions WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .optional()?
            .flatten())
    }

    pub fn sessions_recent(&self, limit: usize) -> anyhow::Result<Vec<Session>> {
        let conn = self.conn();
        let rows = conn
            .prepare(&format!(
                "SELECT {COLS} FROM sessions ORDER BY updated_at DESC LIMIT ?1"
            ))?
            .query_map([limit], row_to_session)?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    pub fn session_update(
        &self,
        id: &str,
        status: Option<SessionStatus>,
        agent_session_id: Option<&str>,
        now: DateTime<Utc>,
    ) -> anyhow::Result<Option<Session>> {
        {
            let conn = self.conn();
            conn.execute(
                "UPDATE sessions SET status = COALESCE(?2, status),
                     agent_session_id = COALESCE(?3, agent_session_id), updated_at = ?4
                 WHERE id = ?1",
                params![id, status.map(|s| s.as_str()), agent_session_id, ts(now)],
            )?;
        }
        self.session_get(id)
    }

    pub fn message_add(
        &self,
        session_id: &str,
        role: &str,
        kind: &str,
        content: &str,
        meta: &serde_json::Value,
        now: DateTime<Utc>,
    ) -> anyhow::Result<SessionMessage> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO session_messages (session_id, role, kind, content, meta, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![session_id, role, kind, content, serde_json::to_string(meta)?, ts(now)],
        )?;
        Ok(SessionMessage {
            id: conn.last_insert_rowid(),
            session_id: session_id.into(),
            role: role.into(),
            kind: kind.into(),
            content: content.into(),
            meta: meta.clone(),
            created_at: now,
        })
    }

    pub fn messages(&self, session_id: &str) -> anyhow::Result<Vec<SessionMessage>> {
        let conn = self.conn();
        let rows = conn
            .prepare(
                "SELECT id, session_id, role, kind, content, meta, created_at FROM session_messages
                 WHERE session_id = ?1 ORDER BY id",
            )?
            .query_map([session_id], |r| {
                Ok(SessionMessage {
                    id: r.get(0)?,
                    session_id: r.get(1)?,
                    role: r.get(2)?,
                    kind: r.get(3)?,
                    content: r.get(4)?,
                    meta: serde_json::from_str(&r.get::<_, String>(5)?).unwrap_or_default(),
                    created_at: parse_ts(&r.get::<_, String>(6)?)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// When each attached file was last part of an active chat: the later of
    /// the message it was sent in and the chat's last activity. Files in a
    /// chat that's mid-turn count as used now.
    pub fn attachments_last_used(
        &self,
        now: DateTime<Utc>,
    ) -> anyhow::Result<std::collections::HashMap<String, DateTime<Utc>>> {
        let conn = self.conn();
        let rows: Vec<(String, String, String, String)> = conn
            .prepare(
                "SELECT m.meta, m.created_at, s.updated_at, s.status
                 FROM session_messages m JOIN sessions s ON s.id = m.session_id
                 WHERE m.meta LIKE '%\"attachments\"%'",
            )?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<Result<_, _>>()?;
        let mut out = std::collections::HashMap::new();
        for (meta, sent, updated, status) in rows {
            let Ok(meta) = serde_json::from_str::<serde_json::Value>(&meta) else {
                continue;
            };
            let Some(paths) = meta.get("attachments").and_then(|a| a.as_array()) else {
                continue;
            };
            let used = if status == "working" {
                now
            } else {
                parse_ts(&sent)?.max(parse_ts(&updated)?)
            };
            for p in paths.iter().filter_map(|p| p.as_str()) {
                let e = out.entry(p.to_string()).or_insert(used);
                *e = (*e).max(used);
            }
        }
        Ok(out)
    }

    /// Ids of chats that have been active since `since` or are mid-turn.
    pub fn sessions_active(
        &self,
        since: DateTime<Utc>,
    ) -> anyhow::Result<std::collections::HashSet<String>> {
        let conn = self.conn();
        let ids = conn
            .prepare("SELECT id FROM sessions WHERE updated_at >= ?1 OR status = 'working'")?
            .query_map([ts(since)], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        Ok(ids)
    }
}
