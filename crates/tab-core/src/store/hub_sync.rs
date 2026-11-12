//! Replicating `tab.db` through the hub.
//!
//! The hub's change log is the source of truth every device syncs from;
//! `tab.db` is this device's copy, with the indexes, full-text search and
//! vectors the app reads. Replication is per row, for the tables in
//! [`SYNCED`]:
//!
//! - **Out:** triggers on each synced table note every changed row in
//!   `hub_outbox`, in the same transaction as the change, so nothing is
//!   missed even if the app quits right after. The hub host drains the
//!   outbox ([`Store::hub_outbox_take`]) into the hub as `Put`/`Delete` ops
//!   carrying the whole row as soon as the transaction commits
//!   ([`Store::commits`]), then acknowledges ([`Store::hub_outbox_done`]).
//! - **In:** the hub's log is read from where this copy last got to
//!   ([`Store::hub_pulled_to`]) and written back with
//!   [`Store::hub_apply_remote`], with the triggers switched off so they
//!   don't echo. That covers other devices' changes, and rebuilds a fresh
//!   or deleted `tab.db` from the hub.
//!
//! Agent chats sync too (`sessions`, `session_messages`), so other devices
//! can watch and steer them; each session names the device that runs it.
//! A message's local `id` only orders this copy, so messages sync by their
//! `uid` and `id` stays here ([`LOCAL_ONLY`]). Only what the person reads
//! in a chat travels: tool output stays here ([`KEEP_LOCAL`]), and a tool
//! call syncs as its one-line summary without its input.
//!
//! What isn't synced stays on this device on purpose: caches (HTTP, AI,
//! previews, embeddings), the API audit log and usage counters.
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
    ("sessions", &["id"]),
    ("session_messages", &["uid"]),
    ("session_segments", &["id"]),
    ("project_notes", &["id"]),
    ("project_repos", &["id"]),
    ("project_resources", &["id"]),
    ("item_context", &["id"]),
    ("entities", &["id"]),
    ("entity_blocks", &["kind", "key"]),
    ("item_entities", &["item_id", "entity_id", "rel"]),
    ("edges", &["src", "dst", "rel"]),
    ("item_activity", &["id"]),
    ("workflows", &["id"]),
    ("workflow_versions", &["workflow_id", "version"]),
    ("workflow_runs", &["id"]),
    ("jobs", &["id"]),
    ("runners", &["id"]),
    ("cloud_connections", &["id"]),
];

/// Columns that only mean something in this copy, so they aren't sent and
/// a row from the hub leaves them to this copy's defaults.
pub const LOCAL_ONLY: &[(&str, &str)] = &[("session_messages", "id")];

/// Rows of a synced table that stay on this device anyway, as a condition
/// on the row (`{r}` is its alias). Tool output is the agent's working
/// material: big, and not something the person reads in a chat on another
/// device. The tool call itself syncs as its one-line summary.
const KEEP_LOCAL: &[(&str, &str)] = &[("session_messages", "{r}.kind = 'tool_result'")];

/// `AND NOT (...)` for the rows of `table` that sync, with alias `r`.
fn synced_rows(table: &str, r: &str) -> String {
    KEEP_LOCAL
        .iter()
        .filter(|(t, _)| *t == table)
        .map(|(_, cond)| format!(" AND NOT ({})", cond.replace("{r}", r)))
        .collect()
}

/// Tool calls whose input is shown to the person as it is (the step list,
/// questions for them), as a condition on `meta`: these sync whole.
const PERSON_FACING_TOOL: &str = "json_extract(meta, '$.tool') IN \
     ('TodoWrite', 'todowrite', 'TaskCreate', 'TaskUpdate', 'AskUserQuestion') \
     OR json_extract(meta, '$.tool') LIKE '%tab_steps'";

/// The most of a chat message's text that's synced. Tool output can be
/// huge; the full text stays on the device that ran it.
pub const MAX_SYNCED_CONTENT: usize = 64 * 1024;

/// The tables synced before `hub_tables` started keeping track; a copy
/// that was introduced then already sent them.
const NEW_TABLES_BASELINE: &[&str] = &[
    "projects",
    "items",
    "project_notes",
    "item_context",
    "entities",
    "entity_blocks",
    "item_entities",
    "edges",
    "item_activity",
    "workflows",
    "workflow_versions",
    "workflow_runs",
];

/// What the Hub page lists as synced: a label and the table it counts.
pub const SYNCED_SUMMARY: &[(&str, &str)] = &[
    ("Queue items", "items"),
    ("Projects", "projects"),
    ("Project notes", "project_notes"),
    ("Project resources", "project_resources"),
    ("Chats", "sessions"),
    ("Knowledge graph", "entities"),
    ("Workflows", "workflows"),
];

/// One line of [`Store::hub_synced_counts`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SyncedCount {
    pub what: &'static str,
    pub count: u64,
}

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
                 backfilled INTEGER NOT NULL DEFAULT 0,
                 pulled_to  INTEGER NOT NULL DEFAULT 0
             );
             INSERT OR IGNORE INTO hub_sync (id) VALUES (1);
             UPDATE hub_sync SET applying = 0;",
        );
        let quiet = "(SELECT applying FROM hub_sync WHERE id = 1) = 0";
        for (table, pk) in SYNCED {
            for (event, row) in [("INSERT", "new"), ("UPDATE", "new"), ("DELETE", "old")] {
                let name = format!("hub_out_{table}_{}", event.to_lowercase());
                sql += &format!(
                    "CREATE TRIGGER IF NOT EXISTS {name} AFTER {event} ON {table} WHEN {quiet}{}
                     BEGIN INSERT INTO hub_outbox (tbl, pk) VALUES ('{table}', {}); END;",
                    synced_rows(table, row),
                    key_expr(pk, row)
                );
            }
        }
        conn.execute_batch(&sql)?;
        // Tables added to SYNCED after this copy was introduced to its hub
        // send what they already hold.
        conn.execute_batch("CREATE TABLE IF NOT EXISTS hub_tables (name TEXT PRIMARY KEY);")?;
        let backfilled: bool =
            conn.query_row("SELECT backfilled FROM hub_sync WHERE id = 1", [], |r| {
                r.get::<_, i64>(0).map(|n| n != 0)
            })?;
        let known: i64 = conn.query_row("SELECT COUNT(*) FROM hub_tables", [], |r| r.get(0))?;
        for (table, pk) in SYNCED {
            let new = conn.execute(
                "INSERT OR IGNORE INTO hub_tables (name) VALUES (?1)",
                [table],
            )? > 0;
            // A copy from before this table existed (it has a hub, and
            // either knew other tables or predates the list) catches up.
            if new && backfilled && (known > 0 || !NEW_TABLES_BASELINE.contains(table)) {
                conn.execute(
                    &format!(
                        "INSERT INTO hub_outbox (tbl, pk) SELECT '{table}', {} FROM {table} AS r WHERE 1{}",
                        key_expr(pk, "r"),
                        synced_rows(table, "r")
                    ),
                    [],
                )?;
            }
        }
        // Added after the first release of this table.
        let has_pulled: bool = conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('hub_sync') WHERE name = 'pulled_to'",
            [],
            |r| r.get::<_, i64>(0).map(|n| n > 0),
        )?;
        if !has_pulled {
            conn.execute_batch(
                "ALTER TABLE hub_sync ADD COLUMN pulled_to INTEGER NOT NULL DEFAULT 0",
            )?;
        }
        Ok(())
    }

    /// How far into the hub's log this copy has read.
    pub fn hub_pulled_to(&self) -> anyhow::Result<u64> {
        let conn = self.conn.lock().unwrap();
        Ok(
            conn.query_row("SELECT pulled_to FROM hub_sync WHERE id = 1", [], |r| {
                r.get::<_, i64>(0)
            })? as u64,
        )
    }

    pub fn hub_set_pulled_to(&self, seq: u64) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE hub_sync SET pulled_to = ?1 WHERE id = 1",
            [seq as i64],
        )?;
        Ok(())
    }

    /// Whether this copy has been introduced to its hub (see
    /// [`Store::hub_backfill_once`]).
    pub fn hub_backfilled(&self) -> anyhow::Result<bool> {
        let conn = self.conn.lock().unwrap();
        Ok(
            conn.query_row("SELECT backfilled FROM hub_sync WHERE id = 1", [], |r| {
                r.get::<_, i64>(0)
            })? != 0,
        )
    }

    pub fn hub_mark_backfilled(&self) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("UPDATE hub_sync SET backfilled = 1 WHERE id = 1", [])?;
        Ok(())
    }

    /// Forgets the hub this copy synced with, for when it was replaced by an
    /// empty or different one: the next start introduces them again.
    pub fn hub_forget(&self) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE hub_sync SET backfilled = 0, pulled_to = 0 WHERE id = 1",
            [],
        )?;
        Ok(())
    }

    /// Every synced row's table and key.
    pub fn hub_local_keys(&self) -> anyhow::Result<Vec<(String, String)>> {
        let conn = self.conn.lock().unwrap();
        let mut out = vec![];
        for (table, pk) in SYNCED {
            let keys: Vec<String> = conn
                .prepare(&format!(
                    "SELECT {} FROM {table} AS r WHERE 1{}",
                    key_expr(pk, "r"),
                    synced_rows(table, "r")
                ))?
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            out.extend(keys.into_iter().map(|k| (table.to_string(), k)));
        }
        Ok(out)
    }

    /// Queues these rows to be sent to the hub as they are now.
    pub fn hub_queue(&self, keys: &[(String, String)]) -> anyhow::Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        for (table, key) in keys {
            tx.execute(
                "INSERT INTO hub_outbox (tbl, pk) VALUES (?1, ?2)",
                [table, key],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Rows changed here that haven't reached the hub yet.
    pub fn hub_pending_keys(&self) -> anyhow::Result<std::collections::HashSet<(String, String)>> {
        let conn = self.conn.lock().unwrap();
        let keys = conn
            .prepare_cached("SELECT DISTINCT tbl, pk FROM hub_outbox")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(keys)
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
                    "INSERT INTO hub_outbox (tbl, pk) SELECT '{table}', {} FROM {table} AS r WHERE 1{}",
                    key_expr(pk, "r"),
                    synced_rows(table, "r")
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
            if pk_of(&table).is_none() {
                continue;
            }
            let row = read_row(&conn, &table, &key)?;
            out.push(OutRow {
                seq,
                table,
                key,
                row,
            });
        }
        Ok(out)
    }

    /// A synced row as the hub would store it, or `None` if it isn't here.
    pub fn hub_row(&self, table: &str, key: &str) -> anyhow::Result<Option<Value>> {
        let conn = self.conn.lock().unwrap();
        read_row(&conn, table, key)
    }

    /// Drops outbox rows up to and including `seq`: the hub has them.
    pub fn hub_outbox_done(&self, seq: i64) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM hub_outbox WHERE seq <= ?1", [seq])?;
        Ok(())
    }

    /// How many of each kind of thing this copy holds, for the Hub page, so
    /// people can see that projects, notes and the graph sync along with the
    /// queue. Every table counted here is in [`SYNCED`].
    pub fn hub_synced_counts(&self) -> anyhow::Result<Vec<SyncedCount>> {
        let conn = self.conn.lock().unwrap();
        SYNCED_SUMMARY
            .iter()
            .map(|(what, table)| {
                let count = conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| {
                    r.get::<_, i64>(0)
                })? as u64;
                Ok(SyncedCount { what, count })
            })
            .collect()
    }

    pub fn hub_outbox_len(&self) -> anyhow::Result<u64> {
        let conn = self.conn.lock().unwrap();
        Ok(conn.query_row("SELECT COUNT(*) FROM hub_outbox", [], |r| {
            r.get::<_, i64>(0)
        })? as u64)
    }

    /// Writes the hub's version of rows: `row` is the whole row, or `None`
    /// to delete it. Rows already the same here are left alone, rows
    /// changed here that the hub hasn't seen yet keep the local change
    /// (it's on its way, and the hub will order it after this), and
    /// unknown tables are ignored (a newer device may sync more). Doesn't
    /// echo back into the outbox. Returns the rows it changed.
    pub fn hub_apply_remote(
        &self,
        changes: &[(String, String, Option<Value>)],
    ) -> anyhow::Result<Vec<(String, String)>> {
        let mut changed = vec![];
        let mut conn = self.conn.lock().unwrap();
        let mut tx = conn.transaction()?;
        tx.execute("UPDATE hub_sync SET applying = 1 WHERE id = 1", [])?;
        // A row whose parent comes later in the batch is tried again after
        // the rest, for as long as that gets anywhere; one that still can't
        // be written is skipped rather than holding up everything after it.
        let mut todo: Vec<&(String, String, Option<Value>)> = changes.iter().collect();
        let mut last_try = false;
        while !todo.is_empty() {
            let before = todo.len();
            let mut retry = vec![];
            for change in todo {
                let (table, key, row) = change;
                let Some(pk) = pk_of(table) else {
                    continue;
                };
                let pending: bool = tx
                    .prepare_cached(
                        "SELECT EXISTS (SELECT 1 FROM hub_outbox WHERE tbl = ?1 AND pk = ?2)",
                    )?
                    .query_row([table, key], |r| r.get(0))?;
                if pending || read_row(&tx, table, key)? == *row {
                    continue;
                }
                let sp = tx.savepoint()?;
                let ours = match (table.as_str(), row) {
                    ("items", Some(row)) => item_states_here(&sp, key, row)?,
                    _ => vec![],
                };
                let mut wrote = write_row(&sp, table, pk, key, row.as_ref());
                if wrote.as_ref().is_err_and(is_unique_conflict) {
                    // Another row here has the same natural key (the same
                    // GitHub notification ingested on two devices, say).
                    // The row later in the hub's log wins. Every device
                    // reads the log in the same order, so they all keep
                    // the same one.
                    wrote = drop_twins(&sp, table, pk, key, row.as_ref())
                        .and_then(|_| write_row(&sp, table, pk, key, row.as_ref()));
                }
                if let (Ok(()), Some(row)) = (&wrote, row.as_ref().filter(|_| table == "items")) {
                    wrote = merge_item_state(&sp, key, row, ours);
                }
                match wrote {
                    Ok(()) => {
                        sp.commit()?;
                        changed.push((table.clone(), key.clone()));
                    }
                    Err(e) if last_try => {
                        drop(sp);
                        tracing::warn!(error = %e, table = %table, key = %key, "hub: couldn't write a row from the hub; skipped");
                    }
                    Err(_) => {
                        drop(sp);
                        retry.push(change);
                    }
                }
            }
            // No progress: one more pass, giving up on what still fails.
            last_try = retry.len() == before;
            todo = retry;
        }
        tx.execute("UPDATE hub_sync SET applying = 0 WHERE id = 1", [])?;
        tx.commit()?;
        if changed.iter().any(|(t, _)| t.starts_with("workflow")) {
            self.workflows_bump();
        }
        Ok(changed)
    }
}

fn is_unique_conflict(e: &anyhow::Error) -> bool {
    matches!(
        e.downcast_ref::<rusqlite::Error>(),
        Some(rusqlite::Error::SqliteFailure(f, _))
            if f.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
    )
}

/// Writes (or deletes) one row as the hub has it.
fn write_row(
    conn: &rusqlite::Connection,
    table: &str,
    pk: &[&str],
    key: &str,
    row: Option<&Value>,
) -> anyhow::Result<()> {
    let keys: Vec<Value> = serde_json::from_str(key)?;
    match row {
        None => {
            let wheres: Vec<String> = pk
                .iter()
                .enumerate()
                .map(|(i, c)| format!("{c} = ?{}", i + 1))
                .collect();
            conn.execute(
                &format!("DELETE FROM {table} WHERE {}", wheres.join(" AND ")),
                params_from_iter(keys.iter().map(to_sql)),
            )?;
        }
        Some(Value::Object(fields)) => {
            let known = columns(conn, table)?;
            let cols: Vec<&String> = known.iter().filter(|c| fields.contains_key(*c)).collect();
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
            conn.execute(
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
    Ok(())
}

fn columns(conn: &rusqlite::Connection, table: &str) -> anyhow::Result<Vec<String>> {
    Ok(conn
        .prepare_cached(&format!("SELECT name FROM pragma_table_info('{table}')"))?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

/// Deletes local rows that share a unique key with `row` but aren't it.
/// Only here: the hub keeps its log, and a device replaying it ends up
/// with the same survivor.
fn drop_twins(
    conn: &rusqlite::Connection,
    table: &str,
    pk: &[&str],
    key: &str,
    row: Option<&Value>,
) -> anyhow::Result<()> {
    let Some(Value::Object(fields)) = row else {
        return Ok(());
    };
    let indexes: Vec<String> = conn
        .prepare(&format!(
            "SELECT name FROM pragma_index_list('{table}') WHERE \"unique\" = 1 AND origin != 'pk'"
        ))?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for index in indexes {
        let cols: Vec<String> = conn
            .prepare(&format!("SELECT name FROM pragma_index_info('{index}')"))?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        if cols.is_empty() || cols.iter().any(|c| !fields.contains_key(c)) {
            continue;
        }
        let wheres: Vec<String> = cols
            .iter()
            .enumerate()
            .map(|(i, c)| format!("{c} = ?{}", i + 1))
            .collect();
        let mut args: Vec<Sql> = cols.iter().map(|c| to_sql(&fields[c.as_str()])).collect();
        args.push(Sql::Text(key.to_string()));
        conn.execute(
            &format!(
                "DELETE FROM {table} WHERE {} AND {} != ?{}",
                wheres.join(" AND "),
                key_expr(pk, table),
                cols.len() + 1
            ),
            params_from_iter(args),
        )?;
    }
    Ok(())
}

/// When an item's state changed, what to, and until when if snoozed.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ItemStateAt {
    at: String,
    state: String,
    snoozed_until: Option<String>,
}

impl ItemStateAt {
    /// `state_at` is missing on rows from before it existed (or from older
    /// apps): an item open since it arrived counts as opened when its
    /// activity happened, anything else as of its last update.
    fn new(
        state: String,
        snoozed_until: Option<String>,
        state_at: Option<String>,
        [occurred_at, created_at, updated_at]: [String; 3],
    ) -> Self {
        let at = state_at.unwrap_or(if state == "open" && updated_at == created_at {
            occurred_at
        } else {
            updated_at
        });
        Self {
            at,
            state,
            snoozed_until,
        }
    }

    fn of_row(row: &Value) -> Self {
        let text = |c: &str| row.get(c).and_then(Value::as_str).map(str::to_string);
        Self::new(
            text("state").unwrap_or_else(|| "open".into()),
            text("snoozed_until"),
            text("state_at"),
            ["occurred_at", "created_at", "updated_at"].map(|c| text(c).unwrap_or_default()),
        )
    }
}

/// The states this copy has for the item in `row`: its own row, and a twin
/// with the same source and external id (from before ids were shared).
fn item_states_here(
    conn: &rusqlite::Connection,
    key: &str,
    row: &Value,
) -> anyhow::Result<Vec<ItemStateAt>> {
    let id = serde_json::from_str::<Vec<Value>>(key)?
        .first()
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_default();
    let text = |c: &str| row.get(c).and_then(Value::as_str).unwrap_or_default();
    Ok(conn
        .prepare_cached(
            "SELECT state, snoozed_until, state_at, occurred_at, created_at, updated_at FROM items
             WHERE id = ?1 OR (source = ?2 AND external_id = ?3)",
        )?
        .query_map(
            rusqlite::params![id, text("source"), text("external_id")],
            |r| {
                Ok(ItemStateAt::new(
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    [r.get(3)?, r.get(4)?, r.get(5)?],
                ))
            },
        )?
        .collect::<rusqlite::Result<_>>()?)
}

/// Keeps the newest state of an item the hub just sent over, rather than
/// whatever the newest row says: a device that brought the item in before
/// it synced would otherwise reopen something marked done elsewhere. When
/// this copy's state wins, it goes back to the hub so the others get it.
/// A tie goes to the hub's row, as rows do, so every device keeps the same.
fn merge_item_state(
    conn: &rusqlite::Connection,
    key: &str,
    row: &Value,
    ours: Vec<ItemStateAt>,
) -> anyhow::Result<()> {
    let theirs = ItemStateAt::of_row(row);
    // Timestamps are all RFC 3339 in UTC with milliseconds, so they sort as text.
    let best = ours.into_iter().fold(
        theirs.clone(),
        |best, s| if s.at > best.at { s } else { best },
    );
    let id: Vec<Value> = serde_json::from_str(key)?;
    conn.execute(
        "UPDATE items SET state = ?2, snoozed_until = ?3, state_at = ?4 WHERE id = ?1",
        rusqlite::params![to_sql(&id[0]), best.state, best.snoozed_until, best.at],
    )?;
    if best != theirs {
        conn.execute(
            "INSERT INTO hub_outbox (tbl, pk) VALUES ('items', ?1)",
            [key],
        )?;
    }
    Ok(())
}

/// A synced row as a JSON object of its columns, or `None` if it's gone.
fn read_row(conn: &rusqlite::Connection, table: &str, key: &str) -> anyhow::Result<Option<Value>> {
    let Some(pk) = pk_of(table) else {
        return Ok(None);
    };
    let cols: Vec<String> = conn
        .prepare_cached(&format!("SELECT name FROM pragma_table_info('{table}')"))?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let object: Vec<String> = cols
        .iter()
        .filter(|c| !LOCAL_ONLY.contains(&(table, c.as_str())))
        .map(|c| match (table, c.as_str()) {
            ("session_messages", "content") => format!(
                "'{c}', CASE WHEN length({c}) > {MAX_SYNCED_CONTENT} \
                 THEN substr({c}, 1, {MAX_SYNCED_CONTENT}) || '\n… (cut off; the full text is on the device that ran it)' \
                 ELSE {c} END"
            ),
            // A tool call travels as its summary (`content`) and tool name;
            // its input (whole files, for an edit) stays here. Except the
            // tools whose input is what the person reads: the step list and
            // questions for them.
            ("session_messages", "meta") => format!(
                "'{c}', CASE WHEN kind = 'tool' AND NOT ({PERSON_FACING_TOOL}) \
                 THEN '{{\"tool\":' || coalesce(json_quote(json_extract({c}, '$.tool')), 'null') || '}}' \
                 ELSE {c} END"
            ),
            _ => format!("'{c}', {c}"),
        })
        .collect();
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
        .query_row([key], |r| r.get(0))
        .optional()?;
    Ok(row.map(|r| serde_json::from_str(&r)).transpose()?)
}

#[cfg(test)]
#[path = "hub_sync_tests.rs"]
mod tests;
