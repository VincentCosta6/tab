//! Moving a hub's data between places: local to cloud when someone signs
//! up, cloud to local when they leave, or between backends.
//!
//! The change log is the whole history, so moving is replaying it in order
//! into an empty store. Each change keeps who made it and when, and gets the
//! same sequence number, so devices that were synced to the old hub can
//! carry on from where they were against the new one.
//!
//! Leases aren't copied. They're short-lived claims on work in progress;
//! devices claim again on the new hub.

use tab_domain::ops::{Applied, Seq};
use tab_store::{HubStore, StoreError};

/// Changes copied per batch.
const BATCH: usize = 500;

#[derive(Debug)]
pub enum MigrateError {
    /// The destination already has data. Migrating into a hub in use would
    /// interleave two histories.
    NotEmpty {
        head: Seq,
    },
    Store(StoreError),
}

impl std::fmt::Display for MigrateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MigrateError::NotEmpty { head } => {
                write!(f, "the destination hub already has {head} changes")
            }
            MigrateError::Store(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for MigrateError {}

impl From<StoreError> for MigrateError {
    fn from(e: StoreError) -> Self {
        MigrateError::Store(e)
    }
}

/// Copies everything in `from` into the empty store `to`, and returns the
/// head both now share. `progress` hears the last sequence copied after
/// each batch.
pub fn copy<A: HubStore, B: HubStore>(
    from: &A,
    to: &mut B,
    mut progress: impl FnMut(Seq),
) -> Result<Seq, MigrateError> {
    let head = to.head()?;
    if head != 0 {
        return Err(MigrateError::NotEmpty { head });
    }
    let mut since = 0;
    loop {
        let batch = from.changes_since(since, BATCH)?;
        let Some(last) = batch.last() else {
            break;
        };
        since = last.seq;
        // Changes made together (same author, same moment) are applied
        // together, which keeps sequence numbers identical.
        for group in batch.chunk_by(|a: &Applied, b: &Applied| a.by == b.by && a.at == b.at) {
            let ops: Vec<_> = group.iter().map(|a| a.op.clone()).collect();
            let applied = to.apply(&group[0].by, &ops, group[0].at)?;
            debug_assert_eq!(applied.last().map(|a| a.seq), group.last().map(|a| a.seq));
        }
        progress(since);
    }
    Ok(to.head()?)
}
