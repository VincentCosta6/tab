//! Runs the blob suite against every file store backend.

use std::sync::atomic::{AtomicUsize, Ordering};

mod mem {
    tab_store::blob_conformance_tests!(tab_store::blob::MemBlobStore::new());
}

/// A fresh folder per test, under cargo's per-target temp dir.
fn temp_dir() -> tab_store::fs_blob::FsBlobStore {
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "tab-blob-conformance/{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    tab_store::fs_blob::FsBlobStore::open(dir).unwrap()
}

mod folder {
    tab_store::blob_conformance_tests!(super::temp_dir());
}

#[test]
fn every_blob_check_is_registered() {
    let mut defined = tab_store::blob_conformance::defined_checks();
    let mut registered = mem::BLOB_CONFORMANCE_CHECKS.to_vec();
    defined.sort_unstable();
    registered.sort_unstable();
    assert_eq!(
        defined, registered,
        "every `pub fn` check in blob_conformance.rs must be listed in blob_conformance_tests!, and nothing else"
    );
}

#[test]
fn folder_files_survive_reopening_and_ignore_stray_files() {
    use tab_store::blob::BlobStore;
    let mut b = temp_dir();
    let root = b.root().to_path_buf();
    let now = chrono::DateTime::from_timestamp(1_800_000_000, 0).unwrap();
    let m = b.put(b"kept", now).unwrap();
    // A half-written upload and an unrelated file are not files.
    let shard = b.path(&m.hash).unwrap().parent().unwrap().to_path_buf();
    std::fs::write(shard.join(".tmp-1-1"), b"partial").unwrap();
    std::fs::write(root.join("README"), b"hi").unwrap();
    drop(b);
    let b = tab_store::fs_blob::FsBlobStore::open(&root).unwrap();
    assert_eq!(b.get(&m.hash).unwrap(), Some(b"kept".to_vec()));
    assert_eq!(b.list("", 10).unwrap(), vec![m]);
}
