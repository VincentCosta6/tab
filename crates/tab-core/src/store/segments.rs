//! Compacted stretches of a chat (`session_segments`) and what the
//! coordinator monitor last looked at (`coordinator_watch`).
//!
//! Compacting doesn't delete anything: the messages stay, and a
//! `compacted` divider message (meta `segment_id`) ends each stretch. The
//! segment keeps the agent session that stretch ran in and the summary the
//! next one starts from.

use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, Row, params};
use serde::Serialize;

use super::{Store, parse_ts, ts};
use crate::model::SessionMessage;

/// Message kind of the divider that ends a stretch.
pub const DIVIDER: &str = "compacted";

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Segment {
    pub id: String,
    pub session_id: String,
    pub project_id: Option<String>,
    pub agent_session_id: Option<String>,
    pub summary: String,
    pub tokens: i64,
    pub messages: i64,
    /// `auto` or `manual`.
    pub reason: String,
    pub started_at: DateTime<Utc>,
    pub ended_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

const COLS: &str = "id, session_id, project_id, agent_session_id, summary, tokens, messages, reason, started_at, ended_at, updated_at";

fn row(r: &Row<'_>) -> rusqlite::Result<Segment> {
    Ok(Segment {
        id: r.get(0)?,
        session_id: r.get(1)?,
        project_id: r.get(2)?,
        agent_session_id: r.get(3)?,
        summary: r.get(4)?,
        tokens: r.get(5)?,
        messages: r.get(6)?,
        reason: r.get(7)?,
        started_at: parse_ts(&r.get::<_, String>(8)?)?,
        ended_at: parse_ts(&r.get::<_, String>(9)?)?,
        updated_at: parse_ts(&r.get::<_, String>(10)?)?,
    })
}

fn message_row(r: &Row<'_>) -> rusqlite::Result<SessionMessage> {
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

const MSG_COLS: &str = "id, session_id, role, kind, content, meta, created_at";

/// What the coordinator monitor last looked at for a project.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WatchState {
    pub seen_at: Option<DateTime<Utc>>,
    pub checked_at: Option<DateTime<Utc>>,
    pub briefed_on: Option<String>,
}

/// Something in a project the coordinator hasn't been told about yet.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectChange {
    pub item_id: String,
    pub title: String,
    pub state: String,
    pub urgency: Option<String>,
    /// It arrived (rather than changed state) since then.
    pub new: bool,
    /// Its chat's status, when the chat finished or needs the person since.
    pub thread: Option<String>,
    pub at: DateTime<Utc>,
}

impl Store {
    /// What changed in a project's queue after `since`: items that arrived
    /// or changed state, and threads that finished, failed or need the
    /// person. Re-ranking and other bookkeeping don't count.
    pub fn project_changes(
        &self,
        project_id: &str,
        since: DateTime<Utc>,
        limit: usize,
    ) -> anyhow::Result<Vec<ProjectChange>> {
        let conn = self.conn();
        let out = conn
            .prepare(
                "SELECT i.id, i.title, i.state, i.urgency, i.created_at > ?2,
                        (SELECT s.status FROM sessions s WHERE s.item_id = i.id AND s.updated_at > ?2
                           AND s.status IN ('idle', 'needs_input', 'failed')
                         ORDER BY s.updated_at DESC LIMIT 1) AS thread,
                        MAX(i.created_at, COALESCE(i.state_at, i.created_at),
                            COALESCE((SELECT MAX(s.updated_at) FROM sessions s WHERE s.item_id = i.id
                               AND s.status IN ('idle', 'needs_input', 'failed')), i.created_at)) AS at
                 FROM items i
                 WHERE i.project_id = ?1
                   AND (i.created_at > ?2 OR COALESCE(i.state_at, '') > ?2 OR thread IS NOT NULL)
                 ORDER BY at DESC LIMIT ?3",
            )?
            .query_map(params![project_id, ts(since), limit as i64], |r| {
                Ok(ProjectChange {
                    item_id: r.get(0)?,
                    title: r.get(1)?,
                    state: r.get(2)?,
                    urgency: r.get(3)?,
                    new: r.get(4)?,
                    thread: r.get(5)?,
                    at: parse_ts(&r.get::<_, String>(6)?)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(out)
    }

    /// The local id of the last divider in a chat, if it was ever compacted.
    fn last_divider(&self, session_id: &str) -> anyhow::Result<Option<i64>> {
        Ok(self.conn().query_row(
            "SELECT MAX(id) FROM session_messages WHERE session_id = ?1 AND kind = ?2",
            params![session_id, DIVIDER],
            |r| r.get(0),
        )?)
    }

    /// The messages since the chat was last compacted (all of them if it
    /// never was), with up to `before` messages from just before the divider.
    pub fn messages_current(
        &self,
        session_id: &str,
        before: usize,
    ) -> anyhow::Result<Vec<SessionMessage>> {
        let Some(divider) = self.last_divider(session_id)? else {
            return self.messages(session_id);
        };
        let conn = self.conn();
        let mut out: Vec<SessionMessage> = conn
            .prepare(&format!(
                "SELECT {MSG_COLS} FROM session_messages
                 WHERE session_id = ?1 AND id < ?2 AND role IN ('user', 'assistant') AND kind = 'text'
                 ORDER BY id DESC LIMIT ?3"
            ))?
            .query_map(params![session_id, divider, before as i64], message_row)?
            .collect::<Result<_, _>>()?;
        out.reverse();
        let rest: Vec<SessionMessage> = conn
            .prepare(&format!(
                "SELECT {MSG_COLS} FROM session_messages WHERE session_id = ?1 AND id >= ?2 ORDER BY id"
            ))?
            .query_map(params![session_id, divider], message_row)?
            .collect::<Result<_, _>>()?;
        out.extend(rest);
        Ok(out)
    }

    /// Ends the chat's current stretch: records it as a segment with
    /// `summary`, and forgets the agent session so the next turn starts a
    /// fresh one. The caller posts the divider naming the segment.
    pub fn segment_close(
        &self,
        session_id: &str,
        project_id: Option<&str>,
        summary: &str,
        tokens: i64,
        reason: &str,
        now: DateTime<Utc>,
    ) -> anyhow::Result<Segment> {
        let divider = self.last_divider(session_id)?;
        let id = uuid::Uuid::new_v4().to_string();
        {
            let mut conn = self.conn();
            let tx = conn.transaction()?;
            let (messages, first): (i64, Option<String>) = tx.query_row(
                "SELECT COUNT(*), MIN(created_at) FROM session_messages
                 WHERE session_id = ?1 AND id > ?2 AND kind != ?3",
                params![session_id, divider.unwrap_or(0), DIVIDER],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            let agent_session_id: Option<String> = tx.query_row(
                "SELECT agent_session_id FROM sessions WHERE id = ?1",
                [session_id],
                |r| r.get(0),
            )?;
            tx.execute(
                &format!(
                    "INSERT INTO session_segments ({COLS}, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10, ?10)"
                ),
                params![
                    id,
                    session_id,
                    project_id,
                    agent_session_id,
                    summary.trim(),
                    tokens,
                    messages,
                    reason,
                    first.unwrap_or_else(|| ts(now)),
                    ts(now)
                ],
            )?;
            tx.execute(
                "UPDATE sessions SET agent_session_id = NULL, updated_at = ?2 WHERE id = ?1",
                params![session_id, ts(now)],
            )?;
            tx.commit()?;
        }
        Ok(self.segment_get(&id)?.expect("just written"))
    }

    pub fn segment_get(&self, id: &str) -> anyhow::Result<Option<Segment>> {
        Ok(self
            .conn()
            .query_row(
                &format!("SELECT {COLS} FROM session_segments WHERE id = ?1"),
                [id],
                row,
            )
            .optional()?)
    }

    /// A chat's segments, newest first.
    pub fn segments(&self, session_id: &str) -> anyhow::Result<Vec<Segment>> {
        let conn = self.conn();
        let out = conn
            .prepare(&format!(
                "SELECT {COLS} FROM session_segments WHERE session_id = ?1 ORDER BY ended_at DESC"
            ))?
            .query_map([session_id], row)?
            .collect::<Result<_, _>>()?;
        Ok(out)
    }

    /// The summary the chat's current stretch started from.
    pub fn carried_forward(&self, session_id: &str) -> anyhow::Result<Option<String>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT summary FROM session_segments WHERE session_id = ?1
                 ORDER BY ended_at DESC LIMIT 1",
                [session_id],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn segment_set_summary(
        &self,
        id: &str,
        summary: &str,
        now: DateTime<Utc>,
    ) -> anyhow::Result<Option<Segment>> {
        self.conn().execute(
            "UPDATE session_segments SET summary = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, summary.trim(), ts(now)],
        )?;
        self.segment_get(id)
    }

    /// The messages of a compacted stretch: after the divider before it, up
    /// to its own.
    pub fn segment_messages(&self, id: &str) -> anyhow::Result<Vec<SessionMessage>> {
        let Some(seg) = self.segment_get(id)? else {
            return Ok(Vec::new());
        };
        let conn = self.conn();
        let end: Option<i64> = conn
            .query_row(
                "SELECT id FROM session_messages WHERE session_id = ?1 AND kind = ?2
                   AND json_extract(meta, '$.segment_id') = ?3",
                params![seg.session_id, DIVIDER, id],
                |r| r.get(0),
            )
            .optional()?;
        let Some(end) = end else {
            return Ok(Vec::new());
        };
        let start: i64 = conn.query_row(
            "SELECT COALESCE(MAX(id), 0) FROM session_messages
             WHERE session_id = ?1 AND kind = ?2 AND id < ?3",
            params![seg.session_id, DIVIDER, end],
            |r| r.get(0),
        )?;
        let out = conn
            .prepare(&format!(
                "SELECT {MSG_COLS} FROM session_messages
                 WHERE session_id = ?1 AND id > ?2 AND id < ?3 ORDER BY id"
            ))?
            .query_map(params![seg.session_id, start, end], message_row)?
            .collect::<Result<_, _>>()?;
        Ok(out)
    }

    /// Segments of project chats whose transcript isn't in search yet.
    pub fn segments_unindexed(&self) -> anyhow::Result<Vec<Segment>> {
        let conn = self.conn();
        let out = conn
            .prepare(&format!(
                "SELECT {COLS} FROM session_segments g
                 WHERE project_id IS NOT NULL
                   AND NOT EXISTS (SELECT 1 FROM resource_chunks c WHERE c.resource_id = g.id)
                   AND NOT EXISTS (SELECT 1 FROM resource_index i WHERE i.resource_id = g.id)
                 ORDER BY ended_at"
            ))?
            .query_map([], row)?
            .collect::<Result<_, _>>()?;
        Ok(out)
    }

    pub fn watch_state(&self, project_id: &str) -> anyhow::Result<WatchState> {
        Ok(self
            .conn()
            .query_row(
                "SELECT seen_at, checked_at, briefed_on FROM coordinator_watch WHERE project_id = ?1",
                [project_id],
                |r| {
                    let t = |s: Option<String>| s.and_then(|s| parse_ts(&s).ok());
                    Ok(WatchState {
                        seen_at: t(r.get(0)?),
                        checked_at: t(r.get(1)?),
                        briefed_on: r.get(2)?,
                    })
                },
            )
            .optional()?
            .unwrap_or_default())
    }

    pub fn watch_save(&self, project_id: &str, w: &WatchState) -> anyhow::Result<()> {
        self.conn().execute(
            "INSERT INTO coordinator_watch (project_id, seen_at, checked_at, briefed_on)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (project_id) DO UPDATE SET seen_at = excluded.seen_at,
                 checked_at = excluded.checked_at, briefed_on = excluded.briefed_on",
            params![
                project_id,
                ts(w.seen_at.unwrap_or_else(Utc::now)),
                w.checked_at.map(ts),
                w.briefed_on
            ],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::AgentKind;

    #[test]
    fn compacting_ends_a_stretch_and_starts_fresh() {
        let s = Store::in_memory().unwrap();
        let now = Utc::now();
        let chat = s
            .session_create_keyed("coordinator:p", AgentKind::ClaudeCode, "C", "/w", now)
            .unwrap();
        s.session_update(&chat.id, None, Some("agent-1"), now)
            .unwrap();
        let say = |text: &str, meta: serde_json::Value| {
            s.message_add(&chat.id, "assistant", "text", text, &meta, now)
                .unwrap();
        };
        say("one", serde_json::Value::Null);
        say("two", serde_json::Value::Null);

        let seg = s
            .segment_close(&chat.id, Some("p"), "Open: refund", 150_000, "auto", now)
            .unwrap();
        assert_eq!(seg.agent_session_id.as_deref(), Some("agent-1"));
        assert_eq!(seg.messages, 2);
        let after = s.session_get(&chat.id).unwrap().unwrap();
        assert_eq!(after.agent_session_id, None, "the next turn starts fresh");
        s.message_add(
            &chat.id,
            "system",
            DIVIDER,
            "",
            &serde_json::json!({ "segment_id": seg.id }),
            now,
        )
        .unwrap();
        say("three", serde_json::Value::Null);

        let words = |m: Vec<SessionMessage>| m.into_iter().map(|m| m.content).collect::<Vec<_>>();
        assert_eq!(words(s.segment_messages(&seg.id).unwrap()), ["one", "two"]);
        assert_eq!(
            words(s.messages_current(&chat.id, 1).unwrap()),
            ["two", "", "three"]
        );
        assert_eq!(
            s.carried_forward(&chat.id).unwrap().as_deref(),
            Some("Open: refund")
        );
        s.segment_set_summary(&seg.id, "Open: nothing", now)
            .unwrap();
        assert_eq!(
            s.carried_forward(&chat.id).unwrap().as_deref(),
            Some("Open: nothing")
        );
        assert_eq!(s.segments_unindexed().unwrap().len(), 1);
    }
}
