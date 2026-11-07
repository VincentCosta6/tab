//! Cleaning up files nothing refers to.
//!
//! A file is kept while any current record refers to it
//! ([`HubStore::blob_refs`]). One that nothing refers to is deleted once it's
//! older than a grace period, which covers a device uploading a file a
//! moment before the record that mentions it reaches the hub (or while it's
//! offline in between). Devices keep their own copies only as a cache, so
//! this is the one place files are really removed.

use chrono::{DateTime, Duration, Utc};
use tab_store::blob::{BlobMeta, BlobStore};
use tab_store::{HubStore, Result};

/// How long an unreferenced file is kept by default.
pub const GRACE: Duration = Duration::days(1);

/// Files listed per page while collecting.
const PAGE: usize = 500;

/// Deletes files no record refers to that were stored before `now - grace`.
/// Returns what it deleted.
pub fn collect<S: HubStore, B: BlobStore>(
    store: &S,
    blobs: &mut B,
    now: DateTime<Utc>,
    grace: Duration,
) -> Result<Vec<BlobMeta>> {
    let keep = store.blob_refs()?;
    let cutoff = now - grace;
    let mut gone = vec![];
    let mut after = String::new();
    loop {
        let page = blobs.list(&after, PAGE)?;
        let Some(last) = page.last() else {
            return Ok(gone);
        };
        after = last.hash.clone();
        for m in page {
            if m.stored_at < cutoff && !keep.contains(&m.hash) && blobs.delete(&m.hash)? {
                gone.push(m);
            }
        }
    }
}
