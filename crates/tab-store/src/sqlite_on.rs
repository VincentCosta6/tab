//! A [`HubStore`] on any SQLite connection.
//!
//! The desktop runs SQLite through rusqlite ([`crate::sqlite`]); a cloud hub
//! runs inside a Durable Object on the object's own SQLite, reachable only
//! through `ctx.storage.sql` from inside the Worker. Both are SQLite, so the
//! store is written once here, against the small [`SqliteConn`] interface,
//! and each host adapts its driver to it. That keeps the two hubs behaving
//! the same: the same tables, the same statements, the same search, and the
//! same conformance suite.
//!
//! Tables and queries come from [`crate::sql`] (sea-query), rendered for
//! SQLite. They're prefixed `hub_` so this can share a database with other
//! tables. Search uses an FTS5 trigram index to find candidates and then
//! checks each term exactly, so it matches [`crate::mem::MemStore`] (and
//! terms shorter than three characters, which trigrams can't index, still
//! work by scanning).
//!
//! Builds for Wasm: no driver, no I/O. The host brings the connection.

use std::collections::BTreeSet;

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use sea_query::{SqliteQueryBuilder, Values};
use tab_domain::lease::{self, Claim, Lease};
use tab_domain::ops::{self, Applied, Edge, Op, Record, SearchHit, Seq};

use crate::sql;
use crate::{HubStore, Result, StoreError, StoreStats};

/// A value bound to or read from a statement. The hub's tables hold only
/// text and integers; `Real` is there for drivers that hand integers back
/// as floating point (JavaScript numbers, in a Durable Object).
#[derive(Debug, Clone, PartialEq)]
pub enum SqlValue {
    Null,
    Int(i64),
    Real(f64),
    Text(String),
}

/// What [`SqliteOn`] needs from a SQLite driver.
///
/// Statements use plain `?` placeholders, bound in order. Every statement inside
/// [`transaction`](SqliteConn::transaction) commits together or not at all.
pub trait SqliteConn {
    /// Runs a statement, ignoring any rows.
    fn execute(&self, sql: &str, args: &[SqlValue]) -> Result<()>;

    /// Runs a statement and returns its rows, each a list of column values.
    fn query(&self, sql: &str, args: &[SqlValue]) -> Result<Vec<Vec<SqlValue>>>;

    /// Runs `f` in one transaction: committed if it returns `Ok`, rolled
    /// back if it returns `Err`.
    fn transaction<T>(&mut self, f: impl FnOnce(&Self) -> Result<T>) -> Result<T>;

    /// Roughly how many bytes the database takes.
    fn size_bytes(&self) -> Result<u64>;
}

/// The hub's state in SQLite, on whatever connection the host brings.
pub struct SqliteOn<C> {
    pub(crate) conn: C,
}

/// SQLite-only: the trigram index over each record's lowercased text,
/// keyed by the record's row id.
const SEARCH_TABLE: &str =
    "CREATE VIRTUAL TABLE IF NOT EXISTS hub_search USING fts5(text, tokenize = 'trigram')";

fn db(e: impl std::fmt::Display) -> StoreError {
    StoreError::Backend(e.to_string())
}

/// sea-query's bound values as [`SqlValue`]s.
fn bind(values: Values) -> Vec<SqlValue> {
    use sea_query::Value as Q;
    values
        .into_iter()
        .map(|v| match v {
            Q::String(Some(s)) => SqlValue::Text(s),
            Q::BigInt(Some(n)) => SqlValue::Int(n),
            // LIMITs.
            Q::BigUnsigned(Some(n)) => SqlValue::Int(i64::try_from(n).unwrap_or(i64::MAX)),
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

fn exec(conn: &impl SqliteConn, s: &impl Stmt) -> Result<()> {
    let (q, v) = s.render();
    conn.execute(&q, &bind(v))
}

fn all<T>(
    conn: &impl SqliteConn,
    s: &impl Stmt,
    f: impl FnMut(Cols) -> Result<T>,
) -> Result<Vec<T>> {
    let (q, v) = s.render();
    conn.query(&q, &bind(v))?
        .into_iter()
        .map(Cols)
        .map(f)
        .collect()
}

fn one<T>(
    conn: &impl SqliteConn,
    s: &impl Stmt,
    f: impl FnMut(Cols) -> Result<T>,
) -> Result<Option<T>> {
    Ok(all(conn, s, f)?.into_iter().next())
}

/// One row, read by column.
struct Cols(Vec<SqlValue>);

impl Cols {
    fn value(&self, i: usize) -> Result<&SqlValue> {
        self.0
            .get(i)
            .ok_or_else(|| db(format!("missing column {i}")))
    }

    fn opt_int(&self, i: usize) -> Result<Option<i64>> {
        match self.value(i)? {
            SqlValue::Null => Ok(None),
            SqlValue::Int(n) => Ok(Some(*n)),
            // Exact as long as it's below 2^53, which sequence numbers,
            // tokens and row ids are.
            SqlValue::Real(f) if f.fract() == 0.0 && f.abs() < 9_007_199_254_740_992.0 => {
                Ok(Some(*f as i64))
            }
            other => Err(db(format!(
                "column {i}: expected an integer, got {other:?}"
            ))),
        }
    }

    fn int(&self, i: usize) -> Result<i64> {
        self.opt_int(i)?
            .ok_or_else(|| db(format!("column {i}: expected an integer, got NULL")))
    }

    fn text(&self, i: usize) -> Result<String> {
        match self.value(i)? {
            SqlValue::Text(s) => Ok(s.clone()),
            other => Err(db(format!("column {i}: expected text, got {other:?}"))),
        }
    }

    fn time(&self, i: usize) -> Result<DateTime<Utc>> {
        parse_ts(&self.text(i)?)
    }

    fn json<T: serde::de::DeserializeOwned>(&self, i: usize) -> Result<T> {
        serde_json::from_str(&self.text(i)?).map_err(|e| db(format!("column {i}: {e}")))
    }
}

/// Fixed-width, so text order is time order.
fn ts(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Nanos, true)
}

fn parse_ts(s: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|e| db(format!("bad time {s:?}: {e}")))
}

fn row_lease(r: Cols) -> Result<Lease> {
    Ok(Lease {
        key: r.text(0)?,
        holder: r.text(1)?,
        token: r.int(2)? as u64,
        expires_at: r.time(3)?,
    })
}

impl<C: SqliteConn> SqliteOn<C> {
    /// Uses an open connection, creating the hub's tables if needed.
    pub fn with(conn: C) -> Result<Self> {
        for t in sql::tables() {
            conn.execute(&t.to_string(SqliteQueryBuilder), &[])?;
        }
        for i in sql::indexes() {
            conn.execute(&i.to_string(SqliteQueryBuilder), &[])?;
        }
        conn.execute(SEARCH_TABLE, &[])?;
        Ok(Self { conn })
    }

    pub(crate) fn lease(&self, key: &str) -> Result<Option<Lease>> {
        one(&self.conn, &sql::get_lease(key), row_lease)
    }
}

fn apply_in<C: SqliteConn>(
    tx: &C,
    by: &str,
    batch: &[Op],
    mut seq: Seq,
    now: DateTime<Utc>,
) -> Result<Vec<Applied>> {
    let at = ts(now);
    let mut out = Vec::with_capacity(batch.len());
    for op in batch {
        seq += 1;
        match op {
            Op::Put { kind, id, body } => {
                let upsert = sql::upsert_record(kind, id, body.to_string(), seq as i64, &at);
                let rid =
                    one(tx, &upsert, |r| r.int(0))?.ok_or_else(|| db("upsert returned no row"))?;
                tx.execute(
                    "DELETE FROM hub_search WHERE rowid = ?",
                    &[SqlValue::Int(rid)],
                )?;
                tx.execute(
                    "INSERT INTO hub_search (rowid, text) VALUES (?, ?)",
                    &[
                        SqlValue::Int(rid),
                        SqlValue::Text(ops::search_text(body).to_lowercase()),
                    ],
                )?;
            }
            Op::Delete { kind, id } => {
                if let Some(rid) = one(tx, &sql::delete_record(kind, id), |r| r.int(0))? {
                    tx.execute(
                        "DELETE FROM hub_search WHERE rowid = ?",
                        &[SqlValue::Int(rid)],
                    )?;
                }
                exec(tx, &sql::delete_edges_of(&ops::node(kind, id)))?;
            }
            Op::Link { from, to, rel } => exec(tx, &sql::insert_edge(from, to, rel))?,
            Op::Unlink { from, to, rel } => exec(tx, &sql::delete_edge(from, to, rel))?,
        }
        let op_json = serde_json::to_string(op).map_err(db)?;
        exec(tx, &sql::insert_log(seq as i64, op_json, by, &at))?;
        out.push(Applied {
            seq,
            op: op.clone(),
            by: by.to_string(),
            at: now,
        });
    }
    Ok(out)
}

impl<C: SqliteConn> HubStore for SqliteOn<C> {
    fn head(&self) -> Result<Seq> {
        let max = one(&self.conn, &sql::head(), |r| r.opt_int(0))?;
        Ok(max.flatten().unwrap_or(0) as Seq)
    }

    fn apply(&mut self, by: &str, batch: &[Op], now: DateTime<Utc>) -> Result<Vec<Applied>> {
        ops::validate(batch)?;
        let seq = self.head()?;
        self.conn
            .transaction(|tx| apply_in(tx, by, batch, seq, now))
    }

    fn changes_since(&self, since: Seq, limit: usize) -> Result<Vec<Applied>> {
        all(
            &self.conn,
            &sql::changes_since(since as i64, limit as u64),
            |r| {
                Ok(Applied {
                    seq: r.int(0)? as Seq,
                    op: r.json(1)?,
                    by: r.text(2)?,
                    at: r.time(3)?,
                })
            },
        )
    }

    fn get(&self, kind: &str, id: &str) -> Result<Option<Record>> {
        one(&self.conn, &sql::get_record(kind, id), |r| {
            Ok(Record {
                kind: r.text(0)?,
                id: r.text(1)?,
                body: r.json(2)?,
                seq: r.int(3)? as Seq,
                updated_at: r.time(4)?,
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
        self.conn.transaction(|tx| {
            let current = one(tx, &sql::get_lease(key), row_lease)?;
            let last = one(tx, &sql::last_token(key), |r| r.int(0))?.unwrap_or(0);
            let c = lease::claim(current.as_ref(), last as u64, key, holder, ttl, now);
            if c.won() {
                let l = c.lease();
                let token = l.token as i64;
                exec(
                    tx,
                    &sql::put_lease(&l.key, &l.holder, token, &ts(l.expires_at)),
                )?;
                exec(tx, &sql::put_token(&l.key, token))?;
            }
            Ok(c)
        })
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
        let mut args: Vec<SqlValue> = Vec::new();
        if !long.is_empty() {
            args.push(SqlValue::Text(long.join(" AND ")));
            q.push_str(" AND hub_search MATCH ?");
        }
        for t in &terms {
            args.push(SqlValue::Text(t.clone()));
            q.push_str(" AND instr(s.text, ?) > 0");
        }
        q.push_str(&format!(" ORDER BY r.seq DESC LIMIT {}", limit as i64));
        self.conn
            .query(&q, &args)?
            .into_iter()
            .map(|r| {
                let r = Cols(r);
                Ok(SearchHit {
                    kind: r.text(0)?,
                    id: r.text(1)?,
                    seq: r.int(2)? as Seq,
                })
            })
            .collect()
    }

    fn graph_neighbors(&self, node: &str, limit: usize) -> Result<Vec<Edge>> {
        all(&self.conn, &sql::neighbors(node, limit as u64), |r| {
            Ok(Edge {
                from: r.text(0)?,
                to: r.text(1)?,
                rel: r.text(2)?,
            })
        })
    }

    fn blob_refs(&self) -> Result<BTreeSet<String>> {
        let bodies = all(&self.conn, &sql::bodies_mentioning_blobs(), |r| {
            r.json::<serde_json::Value>(0)
        })?;
        let mut out = BTreeSet::new();
        for b in bodies {
            out.extend(tab_domain::blob::refs(&b));
        }
        Ok(out)
    }

    fn stats(&self, now: DateTime<Utc>) -> Result<StoreStats> {
        let count = |q: &sea_query::SelectStatement| -> Result<u64> {
            Ok(one(&self.conn, q, |r| r.int(0))?.unwrap_or(0) as u64)
        };
        Ok(StoreStats {
            records: count(&sql::count_records())?,
            edges: count(&sql::count_edges())?,
            live_leases: count(&sql::count_live_leases(&ts(now)))?,
            bytes: self.conn.size_bytes()?,
        })
    }
}
