//! Test-only store backends shared by the integration tests.

/// SQLite the way a Durable Object's `ctx.storage.sql` hands it over:
/// integers come back as JavaScript numbers (floats), and statements can't
/// open transactions themselves (the host wraps them in `transactionSync`).
/// The cloud hub's adapter lives in tab-infra; this keeps the shared store
/// honest about both quirks.
pub mod do_like {
    use tab_store::sqlite_on::{SqlValue, SqliteConn, SqliteOn};
    use tab_store::{Result, StoreError};

    pub struct DoLike(pub rusqlite::Connection);

    fn no_tx(sql: &str) -> Result<()> {
        let s = sql.trim_start().to_ascii_uppercase();
        if [
            "BEGIN",
            "COMMIT",
            "ROLLBACK",
            "SAVEPOINT",
            "RELEASE",
            "PRAGMA",
        ]
        .iter()
        .any(|k| s.starts_with(k))
        {
            return Err(StoreError::Backend(format!(
                "not allowed in a Durable Object: {sql}"
            )));
        }
        Ok(())
    }

    /// `ctx.storage.sql` refuses integers a JavaScript number can't hold
    /// exactly, bound or stored.
    fn js_safe(args: &[SqlValue]) -> Result<()> {
        const MAX: i64 = (1 << 53) - 1;
        if args
            .iter()
            .any(|v| matches!(v, SqlValue::Int(n) if !(-MAX..=MAX).contains(n)))
        {
            return Err(StoreError::Backend(
                "Value outside JavaScript safe integer range".into(),
            ));
        }
        Ok(())
    }

    impl SqliteConn for DoLike {
        fn execute(&self, sql: &str, args: &[SqlValue]) -> Result<()> {
            no_tx(sql)?;
            js_safe(args)?;
            SqliteConn::execute(&self.0, sql, args)
        }

        fn query(&self, sql: &str, args: &[SqlValue]) -> Result<Vec<Vec<SqlValue>>> {
            no_tx(sql)?;
            js_safe(args)?;
            Ok(SqliteConn::query(&self.0, sql, args)?
                .into_iter()
                .map(|row| {
                    row.into_iter()
                        .map(|v| match v {
                            SqlValue::Int(n) => SqlValue::Real(n as f64),
                            v => v,
                        })
                        .collect()
                })
                .collect())
        }

        fn transaction<T>(&mut self, f: impl FnOnce(&Self) -> Result<T>) -> Result<T> {
            let c = &self.0;
            c.execute_batch("SAVEPOINT host")
                .map_err(|e| StoreError::Backend(e.to_string()))?;
            let out = f(self);
            let end = if out.is_ok() {
                "RELEASE host"
            } else {
                "ROLLBACK TO host; RELEASE host"
            };
            self.0
                .execute_batch(end)
                .map_err(|e| StoreError::Backend(e.to_string()))?;
            out
        }

        fn size_bytes(&self) -> Result<u64> {
            self.0.size_bytes()
        }
    }

    pub fn store() -> SqliteOn<DoLike> {
        SqliteOn::with(DoLike(rusqlite::Connection::open_in_memory().unwrap())).unwrap()
    }
}
