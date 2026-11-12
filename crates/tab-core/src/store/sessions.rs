//! Agent sessions and their transcripts.

use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, Row, params};

use super::{Store, parse_ts, ts};
use crate::model::{AgentKind, Session, SessionMessage, SessionStatus};

const COLS: &str = "id, item_id, agent, agent_session_id, title, status, cwd, created_at, updated_at, device, model, opened_at";

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
        device: r.get(9)?,
        model: r.get(10)?,
        opened_at: r
            .get::<_, Option<String>>(11)?
            .map(|t| parse_ts(&t))
            .transpose()?,
    })
}

fn row_to_message(r: &Row<'_>) -> rusqlite::Result<SessionMessage> {
    Ok(SessionMessage {
        id: r.get(0)?,
        session_id: r.get(1)?,
        role: r.get(2)?,
        kind: r.get(3)?,
        content: r.get(4)?,
        meta: serde_json::from_str(&r.get::<_, String>(5)?).unwrap_or_default(),
        created_at: parse_ts(&r.get::<_, String>(6)?)?,
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
        let device = self.device_id()?;
        self.conn().execute(
            "INSERT INTO sessions (id, item_id, agent, title, status, cwd, created_at, updated_at, device, opened_at)
             VALUES (?1, ?2, ?3, ?4, 'idle', ?5, ?6, ?6, ?7, ?6)",
            params![id, item_id, agent.as_str(), title, cwd, ts(now), device],
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

    /// An item's current chat: the one opened last ([`Self::session_open`]).
    pub fn session_for_item(&self, item_id: &str) -> anyhow::Result<Option<Session>> {
        Ok(self.sessions_for_item(item_id)?.into_iter().next())
    }

    /// Every chat an item has had, the current one first.
    pub fn sessions_for_item(&self, item_id: &str) -> anyhow::Result<Vec<Session>> {
        let conn = self.conn();
        let rows = conn
            .prepare(&format!(
                "SELECT {COLS} FROM sessions WHERE item_id = ?1
                 ORDER BY COALESCE(opened_at, created_at) DESC, created_at DESC"
            ))?
            .query_map([item_id], row_to_session)?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// Makes a chat its item's current one again (`/resume`).
    pub fn session_open(&self, id: &str, now: DateTime<Utc>) -> anyhow::Result<Option<Session>> {
        self.conn().execute(
            "UPDATE sessions SET opened_at = ?2 WHERE id = ?1",
            params![id, ts(now)],
        )?;
        self.session_get(id)
    }

    /// The first thing the person said in each chat, for telling them apart.
    pub fn sessions_first_prompt(
        &self,
        ids: &[String],
    ) -> anyhow::Result<std::collections::HashMap<String, (String, usize)>> {
        let conn = self.conn();
        let mut first = conn.prepare(
            "SELECT content FROM session_messages
             WHERE session_id = ?1 AND role = 'user' AND kind = 'text' ORDER BY id LIMIT 1",
        )?;
        let mut count = conn.prepare(
            "SELECT COUNT(*) FROM session_messages WHERE session_id = ?1 AND kind = 'text'",
        )?;
        let mut out = std::collections::HashMap::new();
        for id in ids {
            let text: Option<String> = first.query_row([id], |r| r.get(0)).optional()?;
            let n: usize = count.query_row([id], |r| r.get(0))?;
            out.insert(id.clone(), (text.unwrap_or_default(), n));
        }
        Ok(out)
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

    /// The commit a session last pushed to a branch it created, if it did.
    pub fn session_branch_head(
        &self,
        id: &str,
        repo: &str,
        git_ref: &str,
    ) -> anyhow::Result<Option<String>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT head FROM session_branches WHERE session_id = ?1 AND repo = ?2 AND ref = ?3",
                params![id, repo, git_ref],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Records the commit a session pushed to its own branch.
    pub fn session_branch_set(
        &self,
        id: &str,
        repo: &str,
        git_ref: &str,
        head: &str,
    ) -> anyhow::Result<()> {
        self.conn().execute(
            "INSERT INTO session_branches (session_id, repo, ref, head) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (session_id, repo, ref) DO UPDATE SET head = excluded.head",
            params![id, repo, git_ref, head],
        )?;
        Ok(())
    }

    pub fn session_branch_forget(&self, id: &str, repo: &str, git_ref: &str) -> anyhow::Result<()> {
        self.conn().execute(
            "DELETE FROM session_branches WHERE session_id = ?1 AND repo = ?2 AND ref = ?3",
            params![id, repo, git_ref],
        )?;
        Ok(())
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

    /// Sets the model a chat runs on (`""` for the harness's default).
    pub fn session_set_model(&self, id: &str, model: &str) -> anyhow::Result<Option<Session>> {
        self.conn().execute(
            "UPDATE sessions SET model = ?2 WHERE id = ?1",
            params![id, model.trim()],
        )?;
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
        // Vault values released to an agent are kept masked in what's
        // stored and synced; only this device's tool output has them.
        let content: &str = &if kind == "tool_result" {
            std::borrow::Cow::Borrowed(content)
        } else {
            crate::e2e::redact(content)
        };
        let conn = self.conn();
        conn.execute(
            "INSERT INTO session_messages (session_id, role, kind, content, meta, created_at, uid)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                session_id,
                role,
                kind,
                content,
                serde_json::to_string(meta)?,
                ts(now),
                uuid::Uuid::new_v4().simple().to_string()
            ],
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

    /// The file the newest message naming `path` recorded for it (see
    /// [`crate::named_files`]): how a device shows a file an agent made on
    /// another one.
    pub fn named_file(&self, path: &str) -> anyhow::Result<Option<crate::live::FileRef>> {
        // Messages store `meta` as compact JSON, so the path appears as is.
        let needle = format!(
            "%{}%",
            like_escape(&format!("\"path\":{}", serde_json::to_string(path)?))
        );
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT meta FROM session_messages WHERE meta LIKE ?1 ESCAPE '\\'
             ORDER BY id DESC LIMIT 20",
        )?;
        let metas = stmt
            .query_map([needle], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        for meta in metas {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&meta) else {
                continue;
            };
            let files: Vec<crate::live::FileRef> = v
                .get("files")
                .cloned()
                .and_then(|f| serde_json::from_value(f).ok())
                .unwrap_or_default();
            if let Some(f) = files.into_iter().find(|f| f.path.as_deref() == Some(path)) {
                return Ok(Some(f));
            }
        }
        Ok(None)
    }

    /// A message by its synced key (see migration 013).
    pub fn message_by_uid(&self, uid: &str) -> anyhow::Result<Option<SessionMessage>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT id, session_id, role, kind, content, meta, created_at FROM session_messages
                 WHERE uid = ?1",
                [uid],
                row_to_message,
            )
            .optional()?)
    }

    pub fn messages(&self, session_id: &str) -> anyhow::Result<Vec<SessionMessage>> {
        let conn = self.conn();
        let rows = conn
            .prepare(
                "SELECT id, session_id, role, kind, content, meta, created_at FROM session_messages
                 WHERE session_id = ?1 ORDER BY id",
            )?
            .query_map([session_id], row_to_message)?
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

/// `s` for a LIKE pattern with `ESCAPE '\\'`: its `%`, `_` and `\\` match
/// themselves.
fn like_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn named_file_is_the_newest_message_naming_the_path() {
        let s = Store::in_memory().unwrap();
        let chat = s
            .session_create(None, AgentKind::ClaudeCode, "Shots", "/w", Utc::now())
            .unwrap();
        let file = |hash: &str, path: &str| json!({ "files": [{ "hash": hash, "name": "shot.png", "size": 3, "path": path }] });
        let (old, new) = (
            tab_domain::blob::hash(b"old"),
            tab_domain::blob::hash(b"new"),
        );
        s.message_add(
            &chat.id,
            "assistant",
            "text",
            "`/tmp/a_b/shot.png`",
            &file(&old, "/tmp/a_b/shot.png"),
            Utc::now(),
        )
        .unwrap();
        s.message_add(
            &chat.id,
            "assistant",
            "text",
            "again",
            &file(&new, "/tmp/a_b/shot.png"),
            Utc::now(),
        )
        .unwrap();
        // `_` is a LIKE wildcard; it mustn't match another character.
        s.message_add(
            &chat.id,
            "assistant",
            "text",
            "other",
            &file(&old, "/tmp/aXb/shot.png"),
            Utc::now(),
        )
        .unwrap();
        let got = s.named_file("/tmp/a_b/shot.png").unwrap().unwrap();
        assert_eq!(got.hash, new);
        assert_eq!(s.named_file("/tmp/aYb/shot.png").unwrap(), None);
        assert_eq!(s.named_file("/tmp/a%/shot.png").unwrap(), None);
    }
}

#[cfg(test)]
mod several_chats {
    use super::*;
    use crate::store::Upsert;
    use crate::store::items::tests::ev;
    use chrono::Duration;

    #[test]
    fn a_task_shows_the_chat_opened_last() {
        let s = Store::in_memory().unwrap();
        let t0 = Utc::now();
        let Upsert::Inserted(item) = s.upsert(&ev("1", "Fix login"), t0).unwrap() else {
            panic!()
        };
        let first = s
            .session_create(Some(&item.id), AgentKind::ClaudeCode, "Fix login", "/w", t0)
            .unwrap();
        assert_eq!(first.opened_at, Some(first.created_at));

        // /clear: a newer chat becomes the task's.
        let second = s
            .session_create(
                Some(&item.id),
                AgentKind::ClaudeCode,
                "Fix login",
                "/w",
                t0 + Duration::seconds(1),
            )
            .unwrap();
        assert_eq!(s.session_for_item(&item.id).unwrap().unwrap().id, second.id);

        // The old one carrying on doesn't take it back.
        s.session_update(
            &first.id,
            Some(SessionStatus::Working),
            None,
            t0 + Duration::seconds(5),
        )
        .unwrap();
        assert_eq!(s.session_for_item(&item.id).unwrap().unwrap().id, second.id);

        // /resume does.
        s.session_open(&first.id, t0 + Duration::seconds(9))
            .unwrap();
        let all: Vec<String> = s
            .sessions_for_item(&item.id)
            .unwrap()
            .into_iter()
            .map(|x| x.id)
            .collect();
        assert_eq!(all, [first.id.clone(), second.id.clone()]);

        s.message_add(
            &first.id,
            "user",
            "text",
            "Why does login 500?",
            &serde_json::Value::Null,
            t0,
        )
        .unwrap();
        s.message_add(
            &first.id,
            "assistant",
            "text",
            "Looking.",
            &serde_json::Value::Null,
            t0,
        )
        .unwrap();
        s.message_add(
            &first.id,
            "user",
            "text",
            "Thanks",
            &serde_json::Value::Null,
            t0,
        )
        .unwrap();
        let firsts = s
            .sessions_first_prompt(&[first.id.clone(), second.id.clone()])
            .unwrap();
        assert_eq!(firsts[&first.id], ("Why does login 500?".to_string(), 3));
        assert_eq!(firsts[&second.id], (String::new(), 0));
    }

    #[test]
    fn chats_from_older_copies_count_from_created_at() {
        let s = Store::in_memory().unwrap();
        let t0 = Utc::now();
        let Upsert::Inserted(item) = s.upsert(&ev("1", "Fix login"), t0).unwrap() else {
            panic!()
        };
        let old = s
            .session_create(
                Some(&item.id),
                AgentKind::ClaudeCode,
                "t",
                "/w",
                t0 + Duration::seconds(3),
            )
            .unwrap();
        s.conn()
            .execute(
                "UPDATE sessions SET opened_at = NULL WHERE id = ?1",
                [&old.id],
            )
            .unwrap();
        let newer = s
            .session_create(Some(&item.id), AgentKind::ClaudeCode, "t", "/w", t0)
            .unwrap();
        assert_eq!(s.session_get(&old.id).unwrap().unwrap().opened_at, None);
        assert_eq!(s.session_for_item(&item.id).unwrap().unwrap().id, old.id);
        s.session_open(&newer.id, t0 + Duration::seconds(4))
            .unwrap();
        assert_eq!(s.session_for_item(&item.id).unwrap().unwrap().id, newer.id);
    }
}
