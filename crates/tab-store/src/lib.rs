//! `tab-store` is the storage interface a hub runs on.
//!
//! [`HubStore`] is shaped around what tab does (apply a batch of changes,
//! read changes since a cursor, claim a lease, search, walk the graph)
//! rather than around SQL. Each backend implements those operations the way
//! its database does them best: SQLite here, the Durable Object's SQLite in
//! the cloud, Postgres (with `SKIP LOCKED` leases) if an org needs its own
//! database. The rules that decide what an operation means (lease grants,
//! op validation, search matching) live in `tab-domain`, so backends only
//! store and fetch.
//!
//! Every backend has to pass the same [`conformance`] suite.
//!
//! Backends:
//! - [`mem::MemStore`]: in memory. Tests, and a reference for new backends.
//! - [`sqlite::SqliteStore`] (feature `sqlite`): local SQLite.
//!
//! SQL backends share their tables and queries through [`sql`] (feature
//! `sql`), built with sea-query, so a Postgres backend renders the same
//! statements in its dialect instead of rewriting them.
//!
//! The trait is synchronous on purpose. Both backends we need now are
//! synchronous (rusqlite, and the Durable Object SQL API, which runs inside
//! the object's single thread), and a sync trait builds for Wasm with no
//! async runtime. A network database would sit behind an async adapter.

use chrono::{DateTime, Duration, Utc};
use tab_domain::lease::{Claim, Lease};
use tab_domain::ops::{Applied, Edge, Op, OpError, Record, SearchHit, Seq};

#[cfg(feature = "conformance")]
pub mod conformance;
pub mod mem;
#[cfg(feature = "sql")]
pub mod sql;
#[cfg(feature = "sqlite")]
pub mod sqlite;

#[derive(Debug)]
pub enum StoreError {
    /// The ops failed validation. Nothing was written.
    Invalid(OpError),
    /// The database failed.
    Backend(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Invalid(e) => write!(f, "invalid change: {e}"),
            StoreError::Backend(e) => write!(f, "storage failed: {e}"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<OpError> for StoreError {
    fn from(e: OpError) -> Self {
        StoreError::Invalid(e)
    }
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// Where a hub keeps its state. See the crate docs.
pub trait HubStore {
    /// The latest sequence number, 0 when empty.
    fn head(&self) -> Result<Seq>;

    /// Validates and applies a batch in one transaction, giving each op the
    /// next sequence number. All or nothing.
    fn apply(&mut self, by: &str, ops: &[Op], now: DateTime<Utc>) -> Result<Vec<Applied>>;

    /// Changes after `since`, oldest first, at most `limit`.
    fn changes_since(&self, since: Seq, limit: usize) -> Result<Vec<Applied>>;

    fn get(&self, kind: &str, id: &str) -> Result<Option<Record>>;

    /// Claims or renews `key` for `holder` (see [`tab_domain::lease::claim`]).
    fn claim_lease(
        &mut self,
        key: &str,
        holder: &str,
        ttl: Duration,
        now: DateTime<Utc>,
    ) -> Result<Claim>;

    /// Releases `key` if `holder` holds it. Returns whether it did.
    fn release_lease(&mut self, key: &str, holder: &str, now: DateTime<Utc>) -> Result<bool>;

    /// Removes leases that have lapsed by `now` and returns them.
    fn expire_leases(&mut self, now: DateTime<Utc>) -> Result<Vec<Lease>>;

    /// Records whose text contains every term of `query`, ignoring case
    /// (see [`tab_domain::ops::search_terms`]), newest change first.
    fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>>;

    /// Edges from or to `node`, sorted.
    fn graph_neighbors(&self, node: &str, limit: usize) -> Result<Vec<Edge>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_say_what_failed() {
        let invalid: StoreError = OpError {
            index: 2,
            message: "id is empty".into(),
        }
        .into();
        assert_eq!(invalid.to_string(), "invalid change: op 2: id is empty");
        assert_eq!(
            StoreError::Backend("disk full".into()).to_string(),
            "storage failed: disk full"
        );
    }
}
