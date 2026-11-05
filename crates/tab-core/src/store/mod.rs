//! Local SQLite database: the queue, the knowledge graph, the HTTP cache and
//! the API audit log all live in one file in the app data dir.
//!
//! Search uses FTS5 with the trigram tokenizer, so substring search works
//! ("checko" finds "#incident-checkout") across items and entities.

mod graph;
mod http;
mod context;
mod items;
mod previews;
mod sessions;

use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::Connection;

pub use http::{AuditEntry, AuditQuery, CachedResponse, UsageRow};
pub use items::Upsert;

pub struct Store {
    conn: Mutex<Connection>,
}

/// Ordered migrations; `PRAGMA user_version` records how many have run.
const MIGRATIONS: &[&str] = &[
    include_str!("migrations/001_init.sql"),
    include_str!("migrations/002_tasks_context_sessions.sql"),
    include_str!("migrations/003_previews.sql"),
];

pub(crate) fn ts(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub(crate) fn parse_ts(s: &str) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })
}

/// Turns user input into an FTS5 phrase query, or `None` if it's too short
/// for trigram matching (the caller falls back to LIKE).
pub(crate) fn fts_phrase(q: &str) -> Option<String> {
    let q = q.trim();
    (q.chars().count() >= 3).then(|| format!("\"{}\"", q.replace('"', "\"\"")))
}

impl Store {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        Self::init(Connection::open(path)?)
    }

    pub fn in_memory() -> anyhow::Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> anyhow::Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let version: usize = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        for (i, sql) in MIGRATIONS.iter().enumerate().skip(version) {
            let tx = conn.transaction()?;
            tx.execute_batch(sql)?;
            tx.pragma_update(None, "user_version", i + 1)?;
            tx.commit()?;
        }
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub(crate) fn conn(&self) -> MutexGuard<'_, Connection> {
        // A panic while holding the lock can't leave SQLite half-written, so recover.
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }
}
