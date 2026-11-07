//! Volume and timing checks. Ignored by default and never run in CI; run
//! them in release mode every now and then, and after changing a backend:
//!
//! ```sh
//! scripts/storage-tests.sh --perf
//! # or: cargo test -p tab-store --release --test perf -- --ignored --nocapture
//! ```
//!
//! Only real backends are timed here; the in-memory store is a reference
//! for correctness, not speed. Budgets are generous (several times what a laptop takes) so they catch
//! a lost index or an accidental full scan, not noise.

use std::time::{Duration, Instant};

use chrono::DateTime;
use serde_json::json;
use tab_domain::ops::{MAX_BATCH, Op};
use tab_store::HubStore;
use tab_store::sqlite::SqliteStore;

const ITEMS: usize = 100_000;

fn timed<T>(label: &str, budget: Duration, f: impl FnOnce() -> T) -> T {
    let start = Instant::now();
    let out = f();
    let took = start.elapsed();
    println!("{label:<48} {took:>12.2?}  (budget {budget:?})");
    assert!(
        took <= budget,
        "{label} took {took:?}, over its {budget:?} budget"
    );
    out
}

fn words(i: usize) -> String {
    const W: &[&str] = &[
        "login", "review", "deploy", "invoice", "design", "bug", "page", "sync",
    ];
    format!("{} {} #{i}", W[i % W.len()], W[(i / 7) % W.len()])
}

fn load<S: HubStore>(name: &str, s: &mut S) {
    let now = DateTime::from_timestamp(1_800_000_000, 0).unwrap();
    timed(
        &format!("{name}: apply {ITEMS} items + edges"),
        Duration::from_secs(60),
        || {
            let ops: Vec<Op> = (0..ITEMS)
                .flat_map(|i| {
                    // Every 20th item has an attached file.
                    let mut body = json!({"title": words(i), "repo": "tab", "n": i});
                    if i % 20 == 0 {
                        body["files"] =
                            json!([{ "hash": tab_domain::blob::hash(i.to_string().as_bytes()) }]);
                    }
                    [
                        Op::Put {
                            kind: "item".into(),
                            id: i.to_string(),
                            body,
                        },
                        Op::Link {
                            from: format!("item:{i}"),
                            to: format!("project:{}", i % 50),
                            rel: "in".into(),
                        },
                    ]
                })
                .collect();
            for chunk in ops.chunks(MAX_BATCH) {
                s.apply("perf", chunk, now).unwrap();
            }
        },
    );
    assert_eq!(s.head().unwrap(), (ITEMS * 2) as u64);
}

fn queries<S: HubStore>(name: &str, s: &mut S) {
    timed(
        &format!("{name}: read the whole log in pages"),
        Duration::from_secs(20),
        || {
            let mut since = 0;
            while let Some(last) = s.changes_since(since, 1000).unwrap().last() {
                since = last.seq;
            }
        },
    );
    timed(
        &format!("{name}: 1000 gets"),
        Duration::from_secs(2),
        || {
            for i in (0..ITEMS).step_by(ITEMS / 1000) {
                assert!(s.get("item", &i.to_string()).unwrap().is_some());
            }
        },
    );
    timed(
        &format!("{name}: 50 selective searches"),
        Duration::from_secs(5),
        || {
            for i in 0..50 {
                let hits = s.search(&format!("#{}", 1000 + i * 37), 20).unwrap();
                assert!(!hits.is_empty());
            }
        },
    );
    timed(
        &format!("{name}: 20 broad searches (limit 20)"),
        Duration::from_secs(10),
        || {
            for _ in 0..20 {
                assert_eq!(s.search("login review", 20).unwrap().len(), 20);
            }
        },
    );
    timed(
        &format!("{name}: 1000 neighbor lookups"),
        Duration::from_secs(2),
        || {
            for i in 0..1000 {
                assert_eq!(
                    s.graph_neighbors(&format!("item:{i}"), 10).unwrap().len(),
                    1
                );
            }
        },
    );
    timed(
        &format!("{name}: files referenced by {ITEMS} records"),
        Duration::from_secs(10),
        || assert_eq!(s.blob_refs().unwrap().len(), ITEMS / 20),
    );
    timed(
        &format!("{name}: 10k lease claims + expire"),
        Duration::from_secs(10),
        || {
            let now = DateTime::from_timestamp(1_800_000_000, 0).unwrap();
            for i in 0..10_000 {
                assert!(
                    s.claim_lease(
                        &format!("item:{i}"),
                        "perf",
                        chrono::Duration::seconds(30),
                        now
                    )
                    .unwrap()
                    .won()
                );
            }
            let later = now + chrono::Duration::seconds(31);
            assert_eq!(s.expire_leases(later).unwrap().len(), 10_000);
        },
    );
}

#[test]
#[ignore = "volume test; run with scripts/storage-tests.sh --perf"]
fn sqlite_file_at_volume() {
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("tab-store-perf");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("perf.db");
    let _ = std::fs::remove_file(&path);
    let mut s = SqliteStore::open(&path).unwrap();
    load("sqlite file", &mut s);
    queries("sqlite file", &mut s);
    drop(s);
    let _ = std::fs::remove_file(&path);
}
