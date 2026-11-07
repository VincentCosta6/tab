//! A [`HubStore`] on local SQLite.
//!
//! Tables and queries come from [`crate::sql`] (sea-query), rendered for
//! SQLite and run with rusqlite. They're prefixed `hub_` so this can share a
//! database file with other tables. Search uses an FTS5 trigram index to find candidates and then
//! checks each term exactly, so it matches [`crate::mem::MemStore`] (and
//! terms shorter than three characters, which trigrams can't index, still
//! work by scanning).

use std::path::Path;

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use rusqlite::{Connection, OptionalExtension, Row, params_from_iter};
use sea_query::{SqliteQueryBuilder, Values};
use tab_domain::lease::{self, Claim, Lease};
use tab_domain::ops::{self, Applied, Edge, Op, Record, SearchHit, Seq};

use crate::sql;
use crate::{HubStore, Result, StoreError};

/// SQLite-only: the trigram index over each record's lowercased text,
/// keyed by the record's row id.
const SEARCH_TABLE: &str =
    "CREATE VIRTUAL TABLE IF NOT EXISTS hub_search USING fts5(text, tokenize = 'trigram')";

/// sea-query's bound values as rusqlite values.
fn bind(values: Values) -> Vec<rusqlite::types::Value> {
    use rusqlite::types::Value as V;
    use sea_query::Value as Q;
    values
        .into_iter()
        .map(|v| match v {
            Q::String(Some(s)) => V::Text(s),
            Q::BigInt(Some(n)) => V::Integer(n),
            // LIMITs.
            Q::BigUnsigned(Some(n)) => V::Integer(i64::try_from(n).unwrap_or(i64::MAX)),
            // crate::sql only binds text and integers; anything else is a bug there.
            other => panic!("tab_store::sql bound an unsupported value: {other:?}"),
        })
        .collect()
}

/// Something sea-query can render for SQLite.
trait Stmt {
    fn render(&self) -> (String, Values);
}

macro_rules! stmt {
    ($($t:ty),*) => {$(
        impl Stmt for $t {
            fn render(&self) -> (String, Values) {
                self.build(SqliteQueryBuilder)
            }
        }
    )*};
}
stmt!(
    sea_query::SelectStatement,
    sea_query::InsertStatement,
    sea_query::DeleteStatement
);

fn exec(conn: &Connection, s: &impl Stmt) -> Result<usize> {
    let (q, v) = s.render();
    conn.prepare_cached(&q)
        .and_then(|mut st| st.execute(params_from_iter(bind(v))))
        .map_err(db)
}

fn one<T>(
    conn: &Connection,
    s: &impl Stmt,
    f: impl FnOnce(&Row) -> rusqlite::Result<T>,
) -> Result<Option<T>> {
    let (q, v) = s.render();
    conn.prepare_cached(&q)
        .and_then(|mut st| st.query_row(params_from_iter(bind(v)), f).optional())
        .map_err(db)
}

fn all<T>(
    conn: &Connection,
    s: &impl Stmt,
    f: impl FnMut(&Row) -> rusqlite::Result<T>,
) -> Result<Vec<T>> {
    let (q, v) = s.render();
    let mut st = conn.prepare_cached(&q).map_err(db)?;
    st.query_map(params_from_iter(bind(v)), f)
        .and_then(|rows| rows.collect())
        .map_err(db)
}

pub struct SqliteStore {
    conn: Connection,
}

fn db(e: impl std::fmt::Display) -> StoreError {
    StoreError::Backend(e.to_string())
}

/// Fixed-width, so text order is time order.
fn ts(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Nanos, true)
}

fn parse_ts(s: &str) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })
}

fn json_col<T: serde::de::DeserializeOwned>(s: &str) -> rusqlite::Result<T> {
    serde_json::from_str(s).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
}

fn row_lease(r: &Row) -> rusqlite::Result<Lease> {
    Ok(Lease {
        key: r.get(0)?,
        holder: r.get(1)?,
        token: r.get::<_, i64>(2)? as u64,
        expires_at: parse_ts(&r.get::<_, String>(3)?)?,
    })
}

impl SqliteStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::with(Connection::open(path).map_err(db)?)
    }

    pub fn in_memory() -> Result<Self> {
        Self::with(Connection::open_in_memory().map_err(db)?)
    }

    /// Uses an open connection, creating the hub's tables if needed.
    pub fn with(conn: Connection) -> Result<Self> {
        // Same settings as the desktop's own database: WAL, and fsync at
        // checkpoints rather than every commit. Without this each lease
        // claim waits on the disk (the perf suite caught ~1ms per claim).
        // In-memory databases ignore both.
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(db)?;
        conn.pragma_update(None, "synchronous", "NORMAL")
            .map_err(db)?;
        for t in sql::tables() {
            conn.execute(&t.to_string(SqliteQueryBuilder), [])
                .map_err(db)?;
        }
        for i in sql::indexes() {
            conn.execute(&i.to_string(SqliteQueryBuilder), [])
                .map_err(db)?;
        }
        conn.execute(SEARCH_TABLE, []).map_err(db)?;
        Ok(Self { conn })
    }

    fn lease(&self, key: &str) -> Result<Option<Lease>> {
        one(&self.conn, &sql::get_lease(key), row_lease)
    }
}

impl HubStore for SqliteStore {
    fn head(&self) -> Result<Seq> {
        let max = one(&self.conn, &sql::head(), |r| r.get::<_, Option<i64>>(0))?;
        Ok(max.flatten().unwrap_or(0) as Seq)
    }

    fn apply(&mut self, by: &str, batch: &[Op], now: DateTime<Utc>) -> Result<Vec<Applied>> {
        ops::validate(batch)?;
        let mut seq = self.head()?;
        let at = ts(now);
        let tx = self.conn.transaction().map_err(db)?;
        let mut out = Vec::with_capacity(batch.len());
        for op in batch {
            seq += 1;
            match op {
                Op::Put { kind, id, body } => {
                    let upsert = sql::upsert_record(kind, id, body.to_string(), seq as i64, &at);
                    let rid: i64 = one(&tx, &upsert, |r| r.get(0))?
                        .ok_or_else(|| db("upsert returned no row"))?;
                    tx.execute("DELETE FROM hub_search WHERE rowid = ?1", [rid])
                        .map_err(db)?;
                    tx.execute(
                        "INSERT INTO hub_search (rowid, text) VALUES (?1, ?2)",
                        rusqlite::params![rid, ops::search_text(body).to_lowercase()],
                    )
                    .map_err(db)?;
                }
                Op::Delete { kind, id } => {
                    let rid: Option<i64> = one(&tx, &sql::delete_record(kind, id), |r| r.get(0))?;
                    if let Some(rid) = rid {
                        tx.execute("DELETE FROM hub_search WHERE rowid = ?1", [rid])
                            .map_err(db)?;
                    }
                    exec(&tx, &sql::delete_edges_of(&ops::node(kind, id)))?;
                }
                Op::Link { from, to, rel } => {
                    exec(&tx, &sql::insert_edge(from, to, rel))?;
                }
                Op::Unlink { from, to, rel } => {
                    exec(&tx, &sql::delete_edge(from, to, rel))?;
                }
            }
            let op_json = serde_json::to_string(op).map_err(db)?;
            exec(&tx, &sql::insert_log(seq as i64, op_json, by, &at))?;
            out.push(Applied {
                seq,
                op: op.clone(),
                by: by.to_string(),
                at: now,
            });
        }
        tx.commit().map_err(db)?;
        Ok(out)
    }

    fn changes_since(&self, since: Seq, limit: usize) -> Result<Vec<Applied>> {
        all(
            &self.conn,
            &sql::changes_since(since as i64, limit as u64),
            |r| {
                Ok(Applied {
                    seq: r.get::<_, i64>(0)? as Seq,
                    op: json_col(&r.get::<_, String>(1)?)?,
                    by: r.get(2)?,
                    at: parse_ts(&r.get::<_, String>(3)?)?,
                })
            },
        )
    }

    fn get(&self, kind: &str, id: &str) -> Result<Option<Record>> {
        one(&self.conn, &sql::get_record(kind, id), |r| {
            Ok(Record {
                kind: r.get(0)?,
                id: r.get(1)?,
                body: json_col(&r.get::<_, String>(2)?)?,
                seq: r.get::<_, i64>(3)? as Seq,
                updated_at: parse_ts(&r.get::<_, String>(4)?)?,
            })
        })
    }

    fn claim_lease(
        &mut self,
        key: &str,
        holder: &str,
        ttl: Duration,
        now: DateTime<Utc>,
    ) -> Result<Claim> {
        let tx = self.conn.transaction().map_err(db)?;
        let current = one(&tx, &sql::get_lease(key), row_lease)?;
        let last: i64 = one(&tx, &sql::last_token(key), |r| r.get(0))?.unwrap_or(0);
        let c = lease::claim(current.as_ref(), last as u64, key, holder, ttl, now);
        if c.won() {
            let l = c.lease();
            let token = l.token as i64;
            exec(
                &tx,
                &sql::put_lease(&l.key, &l.holder, token, &ts(l.expires_at)),
            )?;
            exec(&tx, &sql::put_token(&l.key, token))?;
        }
        tx.commit().map_err(db)?;
        Ok(c)
    }

    fn release_lease(&mut self, key: &str, holder: &str, now: DateTime<Utc>) -> Result<bool> {
        if lease::may_release(self.lease(key)?.as_ref(), holder, now) {
            exec(&self.conn, &sql::delete_lease(key))?;
            return Ok(true);
        }
        Ok(false)
    }

    fn expire_leases(&mut self, now: DateTime<Utc>) -> Result<Vec<Lease>> {
        let mut gone = all(&self.conn, &sql::expire_leases(&ts(now)), row_lease)?;
        gone.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(gone)
    }

    fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        let terms = ops::search_terms(query);
        if terms.is_empty() {
            return Ok(vec![]);
        }
        // SQLite-specific: trigrams narrow it down (they need 3+ characters
        // per term), then instr() on the lowercased text decides.
        let long: Vec<String> = terms
            .iter()
            .filter(|t| t.chars().count() >= 3)
            .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
            .collect();
        let mut q = String::from(
            "SELECT r.kind, r.id, r.seq FROM hub_search s \
             JOIN hub_records r ON r.rid = s.rowid WHERE 1",
        );
        let mut args: Vec<String> = Vec::new();
        if !long.is_empty() {
            args.push(long.join(" AND "));
            q.push_str(&format!(" AND hub_search MATCH ?{}", args.len()));
        }
        for t in &terms {
            args.push(t.clone());
            q.push_str(&format!(" AND instr(s.text, ?{}) > 0", args.len()));
        }
        q.push_str(&format!(" ORDER BY r.seq DESC LIMIT {}", limit as i64));
        let mut st = self.conn.prepare(&q).map_err(db)?;
        st.query_map(params_from_iter(args.iter()), |r| {
            Ok(SearchHit {
                kind: r.get(0)?,
                id: r.get(1)?,
                seq: r.get::<_, i64>(2)? as Seq,
            })
        })
        .and_then(|rows| rows.collect())
        .map_err(db)
    }

    fn graph_neighbors(&self, node: &str, limit: usize) -> Result<Vec<Edge>> {
        all(&self.conn, &sql::neighbors(node, limit as u64), |r| {
            Ok(Edge {
                from: r.get(0)?,
                to: r.get(1)?,
                rel: r.get(2)?,
            })
        })
    }
}

/// SQLite-specific behavior the shared suite can't reach: what happens when
/// the file holds something this code didn't write.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::HubStore;
    use serde_json::json;

    fn store_with_item() -> SqliteStore {
        let mut s = SqliteStore::in_memory().unwrap();
        let now = DateTime::from_timestamp(1_800_000_000, 0).unwrap();
        s.apply(
            "mac",
            &[Op::Put {
                kind: "item".into(),
                id: "1".into(),
                body: json!({"t": "x"}),
            }],
            now,
        )
        .unwrap();
        s.claim_lease("k", "mac", Duration::seconds(30), now)
            .unwrap();
        s
    }

    fn backend_error<T: std::fmt::Debug>(r: Result<T>) -> String {
        match r {
            Err(e @ StoreError::Backend(_)) => e.to_string(),
            other => panic!("expected a storage error, got {other:?}"),
        }
    }

    #[test]
    fn corrupt_rows_are_errors_not_panics() {
        let s = store_with_item();
        s.conn
            .execute_batch(
                "UPDATE hub_log SET at = 'yesterday';
                 UPDATE hub_records SET body = '{not json';
                 UPDATE hub_leases SET expires_at = 'soon';",
            )
            .unwrap();
        let e = backend_error(s.changes_since(0, 10));
        assert!(e.starts_with("storage failed:"), "{e}");
        backend_error(s.get("item", "1"));
        backend_error(s.lease("k"));

        let s = store_with_item();
        s.conn
            .execute("UPDATE hub_log SET op = '{\"op\":\"teleport\"}'", [])
            .unwrap();
        backend_error(s.changes_since(0, 10));
    }

    #[test]
    fn a_missing_table_is_an_error() {
        let mut s = store_with_item();
        s.conn.execute_batch("DROP TABLE hub_edges").unwrap();
        backend_error(s.graph_neighbors("a", 10));
        let link = Op::Link {
            from: "a".into(),
            to: "b".into(),
            rel: "r".into(),
        };
        let now = DateTime::from_timestamp(1_800_000_001, 0).unwrap();
        backend_error(s.apply("mac", &[link], now));
        // The failed batch rolled back.
        assert_eq!(s.head().unwrap(), 1);
    }

    #[test]
    fn opening_a_file_that_is_not_a_database_fails() {
        let dir = std::env::temp_dir().join(format!("tab-store-notdb-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("junk.db");
        std::fs::write(&path, b"this is not sqlite, just text that is long enough").unwrap();
        assert!(SqliteStore::open(&path).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
