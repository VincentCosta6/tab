//! Local SQLite database: the queue, the knowledge graph, the HTTP cache and
//! the API audit log all live in one file in the app data dir.
//!
//! Search uses FTS5 with the trigram tokenizer, so substring search works
//! ("checko" finds "#incident-checkout") across items and entities.

mod context;
mod graph;
mod http;
mod items;
mod previews;
mod projects;
mod sessions;
mod usage;
mod vectors;

use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::Connection;

pub use http::{AuditEntry, AuditQuery, CachedResponse, UsageRow};
pub use items::{Scope, Upsert};
pub use usage::{NewUsage, PlanWindow, UsageSummary};
pub use vectors::Doc;

pub struct Store {
    conn: Mutex<Connection>,
    /// Set once the embedding model has loaded; search uses it for queries.
    embedder: std::sync::RwLock<Option<std::sync::Arc<dyn crate::embed::Embed>>>,
    index_status: Mutex<crate::embed::IndexStatus>,
}

/// Ordered migrations; `PRAGMA user_version` records how many have run.
const MIGRATIONS: &[&str] = &[
    include_str!("migrations/001_init.sql"),
    include_str!("migrations/002_tasks_context_sessions.sql"),
    include_str!("migrations/003_previews.sql"),
    include_str!("migrations/004_agent_usage.sql"),
    include_str!("migrations/005_email_previews.sql"),
    include_str!("migrations/006_session_keys.sql"),
    include_str!("migrations/007_projects.sql"),
    include_str!("migrations/008_embeddings.sql"),
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
        vectors::register();
        Self::init(Connection::open(path)?)
    }

    pub fn in_memory() -> anyhow::Result<Self> {
        vectors::register();
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
            embedder: std::sync::RwLock::new(None),
            index_status: Mutex::new(Default::default()),
        })
    }

    pub fn set_embedder(&self, e: std::sync::Arc<dyn crate::embed::Embed>) {
        *self.embedder.write().unwrap_or_else(|e| e.into_inner()) = Some(e);
    }

    pub fn index_status(&self) -> crate::embed::IndexStatus {
        self.index_status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn set_index_status(&self, s: crate::embed::IndexStatus) {
        *self.index_status.lock().unwrap_or_else(|e| e.into_inner()) = s;
    }

    pub fn clear_embedder(&self) {
        *self.embedder.write().unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// The query's vector, once the model is ready.
    pub fn embed_query(&self, q: &str) -> Option<Vec<f32>> {
        let e = self
            .embedder
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()?;
        e.encode(&[q.to_string()]).pop()
    }

    pub(crate) fn conn(&self) -> MutexGuard<'_, Connection> {
        // A panic while holding the lock can't leave SQLite half-written, so recover.
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod migration_order {
    /// Migrations run by position, so numbers must be unique and listed in
    /// order: a new one always goes at the end with the next number.
    #[test]
    fn numbered_once_and_in_order() {
        let src = include_str!("mod.rs");
        let nums: Vec<u32> = src
            .lines()
            .filter_map(|l| l.trim().strip_prefix("include_str!(\"migrations/"))
            .map(|l| l[..3].parse().unwrap())
            .collect();
        assert!(!nums.is_empty());
        assert_eq!(
            nums,
            (1..=nums.len() as u32).collect::<Vec<_>>(),
            "{nums:?}"
        );
    }
}
