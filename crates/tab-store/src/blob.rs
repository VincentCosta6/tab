//! Where a hub keeps files: chat attachments now, encrypted handoff bundles
//! later.
//!
//! Files are content-addressed: [`BlobStore::put`] names them by the hash of
//! their bytes ([`tab_domain::blob::hash`]), so storing the same bytes twice
//! keeps one copy. Records refer to files by that hash, and the hub deletes
//! the ones nothing refers to after a grace period (`tab_hub::blobs`). That's
//! why a file remembers when it was first stored ([`BlobMeta::stored_at`]):
//! a fresh upload whose record hasn't arrived yet is left alone.
//!
//! Like [`crate::HubStore`], the trait is synchronous and every backend has
//! to pass the same suite ([`crate::blob_conformance`]). An R2 backend
//! (the cloud hub) sits behind the Worker's async bindings, and serves
//! downloads through signed URLs rather than through the hub.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use tab_domain::blob::{MAX_BLOB_BYTES, hash, is_hash};
use tab_domain::ops::OpError;

use crate::{Result, StoreError};

/// What a store knows about one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobMeta {
    /// `sha256:<hex>` of the bytes.
    pub hash: String,
    pub size: u64,
    /// When these bytes were first stored. Storing them again doesn't move it.
    pub stored_at: DateTime<Utc>,
}

/// Totals, for the Hub page.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BlobStats {
    pub files: u64,
    pub bytes: u64,
}

pub trait BlobStore {
    /// Stores `bytes` and returns their hash. Storing bytes that are already
    /// there changes nothing and returns the same meta. Refuses files over
    /// [`MAX_BLOB_BYTES`].
    fn put(&mut self, bytes: &[u8], now: DateTime<Utc>) -> Result<BlobMeta>;

    /// The file's bytes, or `None` if it isn't here (or `hash` isn't a hash).
    fn get(&self, hash: &str) -> Result<Option<Vec<u8>>>;

    /// The file's meta without its bytes.
    fn stat(&self, hash: &str) -> Result<Option<BlobMeta>>;

    /// Removes the file. Returns whether it was there.
    fn delete(&mut self, hash: &str) -> Result<bool>;

    /// Files with a hash after `after` (all when empty), in hash order, at
    /// most `limit`. Page by passing the last hash back.
    fn list(&self, after: &str, limit: usize) -> Result<Vec<BlobMeta>>;

    fn stats(&self) -> Result<BlobStats>;
}

/// Checks size; returns the hash.
pub(crate) fn admit(bytes: &[u8]) -> Result<String> {
    if bytes.len() > MAX_BLOB_BYTES {
        return Err(StoreError::Invalid(OpError {
            index: 0,
            message: format!(
                "file is {} bytes; the most a hub takes is {MAX_BLOB_BYTES}",
                bytes.len()
            ),
        }));
    }
    Ok(hash(bytes))
}

/// Files in memory. Tests, and the reference other backends are checked
/// against.
#[derive(Debug, Default, Clone)]
pub struct MemBlobStore {
    files: BTreeMap<String, (BlobMeta, Vec<u8>)>,
}

impl MemBlobStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl BlobStore for MemBlobStore {
    fn put(&mut self, bytes: &[u8], now: DateTime<Utc>) -> Result<BlobMeta> {
        let hash = admit(bytes)?;
        let entry = self.files.entry(hash.clone()).or_insert_with(|| {
            (
                BlobMeta {
                    hash,
                    size: bytes.len() as u64,
                    stored_at: now,
                },
                bytes.to_vec(),
            )
        });
        Ok(entry.0.clone())
    }

    fn get(&self, hash: &str) -> Result<Option<Vec<u8>>> {
        Ok(self.files.get(hash).map(|(_, b)| b.clone()))
    }

    fn stat(&self, hash: &str) -> Result<Option<BlobMeta>> {
        Ok(self.files.get(hash).map(|(m, _)| m.clone()))
    }

    fn delete(&mut self, hash: &str) -> Result<bool> {
        Ok(is_hash(hash) && self.files.remove(hash).is_some())
    }

    fn list(&self, after: &str, limit: usize) -> Result<Vec<BlobMeta>> {
        use std::ops::Bound;
        let start = if after.is_empty() {
            Bound::Unbounded
        } else {
            Bound::Excluded(after.to_string())
        };
        Ok(self
            .files
            .range((start, Bound::Unbounded))
            .take(limit)
            .map(|(_, (m, _))| m.clone())
            .collect())
    }

    fn stats(&self) -> Result<BlobStats> {
        Ok(BlobStats {
            files: self.files.len() as u64,
            bytes: self.files.values().map(|(m, _)| m.size).sum(),
        })
    }
}
