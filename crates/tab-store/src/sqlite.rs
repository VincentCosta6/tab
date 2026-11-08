//! A [`HubStore`] on local SQLite, through rusqlite: the desktop's hub.
//!
//! The store itself is [`crate::sqlite_on::SqliteOn`], shared with the
//! cloud hub; this is the rusqlite side of [`SqliteConn`], plus opening a
//! file with the desktop's settings.

use std::path::Path;

use rusqlite::types::{Value as V, ValueRef};
use rusqlite::{Connection, params_from_iter};

use crate::sqlite_on::{SqlValue, SqliteConn, SqliteOn};
use crate::{Result, StoreError};

/// The hub's store on a rusqlite connection.
pub type SqliteStore = SqliteOn<Connection>;

fn db(e: impl std::fmt::Display) -> StoreError {
    StoreError::Backend(e.to_string())
}

fn to_sql(args: &[SqlValue]) -> impl Iterator<Item = V> + '_ {
    args.iter().map(|a| match a {
        SqlValue::Null => V::Null,
        SqlValue::Int(n) => V::Integer(*n),
        SqlValue::Real(f) => V::Real(*f),
        SqlValue::Text(s) => V::Text(s.clone()),
    })
}

fn from_sql(v: ValueRef) -> Result<SqlValue> {
    Ok(match v {
        ValueRef::Null => SqlValue::Null,
        ValueRef::Integer(n) => SqlValue::Int(n),
        ValueRef::Real(f) => SqlValue::Real(f),
        ValueRef::Text(t) => SqlValue::Text(std::str::from_utf8(t).map_err(db)?.to_string()),
        ValueRef::Blob(_) => return Err(db("unexpected blob column")),
    })
}

impl SqliteConn for Connection {
    fn execute(&self, sql: &str, args: &[SqlValue]) -> Result<()> {
        self.prepare_cached(sql)
            .and_then(|mut st| st.execute(params_from_iter(to_sql(args))))
            .map(|_| ())
            .map_err(db)
    }

    fn query(&self, sql: &str, args: &[SqlValue]) -> Result<Vec<Vec<SqlValue>>> {
        let mut st = self.prepare_cached(sql).map_err(db)?;
        let n = st.column_count();
        let mut rows = st.query(params_from_iter(to_sql(args))).map_err(db)?;
        let mut out = Vec::new();
        while let Some(r) = rows.next().map_err(db)? {
            out.push(
                (0..n)
                    .map(|i| from_sql(r.get_ref(i).map_err(db)?))
                    .collect::<Result<_>>()?,
            );
        }
        Ok(out)
    }

    fn transaction<T>(&mut self, f: impl FnOnce(&Self) -> Result<T>) -> Result<T> {
        let tx = Connection::transaction(self).map_err(db)?;
        let out = f(&tx)?;
        tx.commit().map_err(db)?;
        Ok(out)
    }

    fn size_bytes(&self) -> Result<u64> {
        // The file's size in pages.
        let bytes: i64 = self
            .query_row(
                "SELECT page_count * page_size FROM pragma_page_count(), pragma_page_size()",
                [],
                |r| r.get(0),
            )
            .map_err(db)?;
        Ok(bytes as u64)
    }
}

impl SqliteOn<Connection> {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::local(Connection::open(path).map_err(db)?)
    }

    pub fn in_memory() -> Result<Self> {
        Self::local(Connection::open_in_memory().map_err(db)?)
    }

    /// Uses an open local connection with the desktop's settings, creating
    /// the hub's tables if needed.
    pub fn local(conn: Connection) -> Result<Self> {
        // Same settings as the desktop's own database: WAL, and fsync at
        // checkpoints rather than every commit. Without this each lease
        // claim waits on the disk (the perf suite caught ~1ms per claim).
        // In-memory databases ignore both.
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(db)?;
        conn.pragma_update(None, "synchronous", "NORMAL")
            .map_err(db)?;
        Self::with(conn)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HubStore;
    use chrono::{DateTime, Duration};
    use serde_json::json;
    use tab_domain::ops::Op;

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
    fn columns_of_the_wrong_type_are_errors() {
        for (bad, says) in [
            ("UPDATE hub_records SET seq = 'soon'", "expected an integer"),
            ("UPDATE hub_records SET seq = 1.5", "expected an integer"),
            ("UPDATE hub_records SET kind = X'00'", "blob"),
        ] {
            let s = store_with_item();
            s.conn.execute_batch(bad).unwrap();
            let e = match s.get("item", "1") {
                // The blob kind no longer matches, so look it up by scan.
                Ok(None) => backend_error(s.search("x", 5)),
                r => backend_error(r),
            };
            assert!(e.contains(says), "{bad}: {e}");
        }
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
