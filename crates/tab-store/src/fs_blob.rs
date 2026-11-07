//! Files in a local folder: the desktop hub's [`BlobStore`].
//!
//! Each file is `<root>/<first two hex digits>/<64 hex digits>`, written to
//! a temporary name and renamed into place, so a crash never leaves half a
//! file under a real name. When it was first stored is the file's
//! modification time, which re-storing the same bytes doesn't touch.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{DateTime, Utc};
use tab_domain::blob::{from_hex, hex};

use crate::blob::{BlobMeta, BlobStats, BlobStore, admit};
use crate::{Result, StoreError};

pub struct FsBlobStore {
    root: PathBuf,
}

fn io(e: std::io::Error) -> StoreError {
    StoreError::Backend(e.to_string())
}

static TMP: AtomicU64 = AtomicU64::new(0);

impl FsBlobStore {
    /// Opens (or creates) the folder.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        fs::create_dir_all(&root).map_err(io)?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Where the file with this hash lives (whether or not it's there), so
    /// the desktop can link it into its attachments folder instead of
    /// copying. `None` if `hash` isn't a hash.
    pub fn path(&self, hash: &str) -> Option<PathBuf> {
        let hex = hex(hash)?;
        Some(self.root.join(&hex[..2]).join(hex))
    }

    fn meta(&self, hash: &str, path: &Path) -> Result<Option<BlobMeta>> {
        match fs::metadata(path) {
            Ok(m) if m.is_file() => Ok(Some(BlobMeta {
                hash: hash.to_string(),
                size: m.len(),
                stored_at: m.modified().map_err(io)?.into(),
            })),
            Ok(_) => Ok(None),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io(e)),
        }
    }
}

/// Sets a file's modification time (its "stored at").
fn set_mtime(path: &Path, at: DateTime<Utc>) -> std::io::Result<()> {
    let f = fs::File::options().write(true).open(path)?;
    f.set_modified(at.into())
}

impl BlobStore for FsBlobStore {
    fn put(&mut self, bytes: &[u8], now: DateTime<Utc>) -> Result<BlobMeta> {
        let hash = admit(bytes)?;
        let path = self.path(&hash).expect("admit returns a hash");
        if let Some(m) = self.meta(&hash, &path)? {
            return Ok(m);
        }
        let dir = path.parent().expect("has a shard folder");
        fs::create_dir_all(dir).map_err(io)?;
        let tmp = dir.join(format!(
            ".tmp-{}-{}",
            std::process::id(),
            TMP.fetch_add(1, Ordering::Relaxed)
        ));
        let written = (|| {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(bytes)?;
            f.sync_all()?;
            drop(f);
            set_mtime(&tmp, now)?;
            fs::rename(&tmp, &path)
        })();
        if let Err(e) = written {
            let _ = fs::remove_file(&tmp);
            return Err(io(e));
        }
        Ok(self.meta(&hash, &path)?.expect("just written"))
    }

    fn get(&self, hash: &str) -> Result<Option<Vec<u8>>> {
        let Some(path) = self.path(hash) else {
            return Ok(None);
        };
        match fs::read(&path) {
            Ok(b) => Ok(Some(b)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io(e)),
        }
    }

    fn stat(&self, hash: &str) -> Result<Option<BlobMeta>> {
        match self.path(hash) {
            Some(p) => self.meta(hash, &p),
            None => Ok(None),
        }
    }

    fn delete(&mut self, hash: &str) -> Result<bool> {
        let Some(path) = self.path(hash) else {
            return Ok(false);
        };
        match fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(io(e)),
        }
    }

    fn list(&self, after: &str, limit: usize) -> Result<Vec<BlobMeta>> {
        let mut out = Vec::new();
        if limit == 0 {
            return Ok(out);
        }
        let after_hex = hex(after).unwrap_or("");
        let mut shards: Vec<String> = fs::read_dir(&self.root)
            .map_err(io)?
            .flatten()
            .filter(|e| e.path().is_dir())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|s| s.len() == 2 && after_hex.get(..2).is_none_or(|a| s.as_str() >= a))
            .collect();
        shards.sort_unstable();
        for shard in shards {
            let mut names: Vec<String> = fs::read_dir(self.root.join(&shard))
                .map_err(io)?
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.as_str() > after_hex && n.starts_with(&shard))
                .collect();
            names.sort_unstable();
            for name in names {
                let Some(hash) = from_hex(&name) else {
                    continue;
                };
                if let Some(m) = self.meta(&hash, &self.root.join(&shard).join(&name))? {
                    out.push(m);
                    if out.len() == limit {
                        return Ok(out);
                    }
                }
            }
        }
        Ok(out)
    }

    fn stats(&self) -> Result<BlobStats> {
        let mut s = BlobStats::default();
        let mut after = String::new();
        loop {
            let page = self.list(&after, 1000)?;
            let Some(last) = page.last() else {
                return Ok(s);
            };
            after = last.hash.clone();
            s.files += page.len() as u64;
            s.bytes += page.iter().map(|m| m.size).sum::<u64>();
        }
    }
}
