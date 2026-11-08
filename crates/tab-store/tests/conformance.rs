//! Every conformance check against every backend in this crate. Run with
//! `cargo test -p tab-store` (or `scripts/storage-tests.sh` for everything).

use std::sync::atomic::{AtomicUsize, Ordering};

mod mem {
    tab_store::conformance_tests!(tab_store::mem::MemStore::new());
}

mod sqlite_in_memory {
    tab_store::conformance_tests!(tab_store::sqlite::SqliteStore::in_memory().unwrap());
}

/// A fresh database file per test, under cargo's per-target temp dir.
fn temp_db() -> tab_store::sqlite::SqliteStore {
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("tab-store-conformance");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!(
        "{}-{}.db",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_file(&path);
    tab_store::sqlite::SqliteStore::open(path).unwrap()
}

mod common;

mod like_durable_object {
    tab_store::conformance_tests!(super::common::do_like::store());
}

mod sqlite_file {
    tab_store::conformance_tests!(super::temp_db());
}

#[test]
fn every_check_is_registered() {
    let mut defined = tab_store::conformance::defined_checks();
    let mut registered = mem::CONFORMANCE_CHECKS.to_vec();
    defined.sort_unstable();
    registered.sort_unstable();
    assert_eq!(
        defined, registered,
        "every `pub fn` check in conformance.rs must be listed in conformance_tests!, and nothing else"
    );
}

#[test]
fn sqlite_data_survives_reopening() {
    use tab_store::HubStore;
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("tab-store-reopen");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let now = chrono::DateTime::from_timestamp(1_800_000_000, 0).unwrap();
    {
        let mut s = tab_store::sqlite::SqliteStore::open(&path).unwrap();
        s.apply(
            "mac",
            &[
                tab_domain::ops::Op::Put {
                    kind: "item".into(),
                    id: "1".into(),
                    body: serde_json::json!({"title": "kept"}),
                },
                tab_domain::ops::Op::Link {
                    from: "item:1".into(),
                    to: "p".into(),
                    rel: "in".into(),
                },
            ],
            now,
        )
        .unwrap();
        s.claim_lease("k", "mac", chrono::Duration::seconds(60), now)
            .unwrap();
    }
    let mut s = tab_store::sqlite::SqliteStore::open(&path).unwrap();
    assert_eq!(s.head().unwrap(), 2);
    assert_eq!(s.search("kept", 5).unwrap().len(), 1);
    assert_eq!(s.graph_neighbors("p", 5).unwrap().len(), 1);
    assert!(
        !s.claim_lease("k", "phone", chrono::Duration::seconds(60), now)
            .unwrap()
            .won()
    );
    let _ = std::fs::remove_file(&path);
}
