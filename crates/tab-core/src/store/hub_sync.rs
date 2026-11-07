//! Replicating `tab.db` through the hub.
//!
//! The hub's change log is the record every device syncs from; `tab.db` is
//! this device's copy, with the indexes, full-text search and vectors the
//! app reads. Replication is per row, for the tables in [`SYNCED`]:
//!
//! - **Out:** triggers on each synced table note every changed row in
//!   `hub_outbox`, in the same transaction as the change, so nothing is
//!   missed even if the app quits right after. The hub host drains the
//!   outbox ([`Store::hub_outbox_take`]) into the hub as `Put`/`Delete` ops
//!   carrying the whole row, then acknowledges ([`Store::hub_outbox_done`]).
//! - **In:** changes other devices made arrive as the same ops and are
//!   written back with [`Store::hub_apply_remote`], with the triggers
//!   switched off so they don't echo.
//!
//! What isn't synced stays on this device on purpose: caches (HTTP, AI,
//! previews, embeddings), the API audit log, usage counters, and agent
//! chats, which belong to the machine that runs the agent.
//!
//! Every row change is recorded as a whole-row `Put` (or a `Delete`), and
//! the last write the hub orders wins.

use rusqlite::types::Value as Sql;
use rusqlite::{OptionalExtension, params_from_iter};
use serde_json::Value;

use super::Store;

/// Synced tables and their primary-key columns, parents before children.
pub const SYNCED: &[(&str, &[&str])] = &[
    ("projects", &["id"]),
    ("items", &["id"]),
    ("project_notes", &["id"]),
    ("item_context", &["id"]),
    ("entities", &["id"]),
    ("entity_blocks", &["kind", "key"]),
    ("item_entities", &["item_id", "entity_id", "rel"]),
    ("edges", &["src", "dst", "rel"]),
    ("item_activity", &["id"]),
    ("workflows", &["id"]),
    ("workflow_versions", &["workflow_id", "version"]),
    ("workflow_runs", &["id"]),
];

/// One row to send to the hub: its current contents, or `None` if it's gone.
#[derive(Debug, Clone, PartialEq)]
pub struct OutRow {
    pub seq: i64,
    pub table: String,
    /// The primary key as a JSON array, e.g. `["abc"]` or `["i1", 4, "about"]`.
    pub key: String,
    pub row: Option<Value>,
}

fn pk_of(table: &str) -> Option<&'static [&'static str]> {
    SYNCED.iter().find(|(t, _)| *t == table).map(|(_, pk)| *pk)
}

fn key_expr(pk: &[&str], row: &str) -> String {
    let cols: Vec<String> = pk.iter().map(|c| format!("{row}.{c}")).collect();
    format!("json_array({})", cols.join(", "))
}

fn to_sql(v: &Value) -> Sql {
    match v {
        Value::Null => Sql::Null,
        Value::Bool(b) => Sql::Integer(*b as i64),
        Value::Number(n) => n
            .as_i64()
            .map(Sql::Integer)
            .unwrap_or_else(|| Sql::Real(n.as_f64().unwrap_or(0.0))),
        Value::String(s) => Sql::Text(s.clone()),
        other => Sql::Text(other.to_string()),
    }
}

impl Store {
    /// Creates the outbox and the triggers that fill it. Safe to run on
    /// every launch.
    pub fn hub_sync_install(&self) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        let mut sql = String::from(
            "CREATE TABLE IF NOT EXISTS hub_outbox (
                 seq INTEGER PRIMARY KEY AUTOINCREMENT,
                 tbl TEXT NOT NULL,
                 pk  TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS hub_sync (
                 id         INTEGER PRIMARY KEY CHECK (id = 1),
                 applying   INTEGER NOT NULL DEFAULT 0,
                 backfilled INTEGER NOT NULL DEFAULT 0
             );
             INSERT OR IGNORE INTO hub_sync (id) VALUES (1);
             UPDATE hub_sync SET applying = 0;",
        );
        let quiet = "(SELECT applying FROM hub_sync WHERE id = 1) = 0";
        for (table, pk) in SYNCED {
            for (event, row) in [("INSERT", "new"), ("UPDATE", "new"), ("DELETE", "old")] {
                let name = format!("hub_out_{table}_{}", event.to_lowercase());
                sql += &format!(
                    "CREATE TRIGGER IF NOT EXISTS {name} AFTER {event} ON {table} WHEN {quiet}
                     BEGIN INSERT INTO hub_outbox (tbl, pk) VALUES ('{table}', {}); END;",
                    key_expr(pk, row)
                );
            }
        }
        conn.execute_batch(&sql)?;
        Ok(())
    }

    /// The first time, queues every existing synced row, so a hub starting
    /// empty gets everything already in `tab.db`. Returns how many.
    pub fn hub_backfill_once(&self) -> anyhow::Result<usize> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let done: i64 = tx.query_row("SELECT backfilled FROM hub_sync WHERE id = 1", [], |r| {
            r.get(0)
        })?;
        if done != 0 {
            return Ok(0);
        }
        let mut n = 0;
        for (table, pk) in SYNCED {
            n += tx.execute(
                &format!(
                    "INSERT INTO hub_outbox (tbl, pk) SELECT '{table}', {} FROM {table} AS r",
                    key_expr(pk, "r")
                ),
                [],
            )?;
        }
        tx.execute("UPDATE hub_sync SET backfilled = 1 WHERE id = 1", [])?;
        tx.commit()?;
        Ok(n)
    }

    /// Up to `limit` outbox rows, oldest first, each with the row's current
    /// contents. A row changed several times shows up several times with
    /// the same (latest) contents; the hub skips repeats.
    pub fn hub_outbox_take(&self, limit: usize) -> anyhow::Result<Vec<OutRow>> {
        let conn = self.conn.lock().unwrap();
        let pending: Vec<(i64, String, String)> = conn
            .prepare_cached("SELECT seq, tbl, pk FROM hub_outbox ORDER BY seq LIMIT ?1")?
            .query_map([limit as i64], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let mut out = Vec::with_capacity(pending.len());
        for (seq, table, key) in pending {
            let Some(pk) = pk_of(&table) else {
                continue;
            };
            let cols: Vec<String> = conn
                .prepare_cached(&format!("SELECT name FROM pragma_table_info('{table}')"))?
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            let object: Vec<String> = cols.iter().map(|c| format!("'{c}', {c}")).collect();
            let wheres: Vec<String> = pk
                .iter()
                .enumerate()
                .map(|(i, c)| format!("{c} = json_extract(?1, '$[{i}]')"))
                .collect();
            let row: Option<String> = conn
                .prepare_cached(&format!(
                    "SELECT json_object({}) FROM {table} WHERE {}",
                    object.join(", "),
                    wheres.join(" AND ")
                ))?
                .query_row([&key], |r| r.get(0))
                .optional()?;
            out.push(OutRow {
                seq,
                table,
                key,
                row: row.map(|r| serde_json::from_str(&r)).transpose()?,
            });
        }
        Ok(out)
    }

    /// Drops outbox rows up to and including `seq`: the hub has them.
    pub fn hub_outbox_done(&self, seq: i64) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM hub_outbox WHERE seq <= ?1", [seq])?;
        Ok(())
    }

    pub fn hub_outbox_len(&self) -> anyhow::Result<u64> {
        let conn = self.conn.lock().unwrap();
        Ok(conn.query_row("SELECT COUNT(*) FROM hub_outbox", [], |r| {
            r.get::<_, i64>(0)
        })? as u64)
    }

    /// Writes changes another device made: `row` is the whole row, or
    /// `None` to delete it. Unknown tables are ignored (a newer device may
    /// sync more). Doesn't echo back into the outbox.
    pub fn hub_apply_remote(
        &self,
        changes: &[(String, String, Option<Value>)],
    ) -> anyhow::Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        // Children can arrive before parents within a batch.
        tx.pragma_update(None, "defer_foreign_keys", "ON")?;
        tx.execute("UPDATE hub_sync SET applying = 1 WHERE id = 1", [])?;
        for (table, key, row) in changes {
            let Some(pk) = pk_of(table) else {
                continue;
            };
            let keys: Vec<Value> = serde_json::from_str(key)?;
            match row {
                None => {
                    let wheres: Vec<String> = pk
                        .iter()
                        .enumerate()
                        .map(|(i, c)| format!("{c} = ?{}", i + 1))
                        .collect();
                    tx.execute(
                        &format!("DELETE FROM {table} WHERE {}", wheres.join(" AND ")),
                        params_from_iter(keys.iter().map(to_sql)),
                    )?;
                }
                Some(Value::Object(fields)) => {
                    let known: Vec<String> = tx
                        .prepare_cached(&format!("SELECT name FROM pragma_table_info('{table}')"))?
                        .query_map([], |r| r.get(0))?
                        .collect::<rusqlite::Result<_>>()?;
                    let cols: Vec<&String> =
                        known.iter().filter(|c| fields.contains_key(*c)).collect();
                    let names: Vec<&str> = cols.iter().map(|c| c.as_str()).collect();
                    let marks: Vec<String> = (1..=cols.len()).map(|i| format!("?{i}")).collect();
                    let sets: Vec<String> = names
                        .iter()
                        .filter(|c| !pk.contains(c))
                        .map(|c| format!("{c} = excluded.{c}"))
                        .collect();
                    let conflict = if sets.is_empty() {
                        "DO NOTHING".to_string()
                    } else {
                        format!("DO UPDATE SET {}", sets.join(", "))
                    };
                    tx.execute(
                        &format!(
                            "INSERT INTO {table} ({}) VALUES ({}) ON CONFLICT ({}) {conflict}",
                            names.join(", "),
                            marks.join(", "),
                            pk.join(", ")
                        ),
                        params_from_iter(cols.iter().map(|c| to_sql(&fields[c.as_str()]))),
                    )?;
                }
                Some(other) => anyhow::bail!("row for {table} isn't an object: {other}"),
            }
        }
        tx.execute("UPDATE hub_sync SET applying = 0 WHERE id = 1", [])?;
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "hub_sync_tests.rs"]
mod tests;
