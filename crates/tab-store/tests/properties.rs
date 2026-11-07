//! Property tests: random workloads run against every backend at once, and
//! they must agree on everything observable. Plus invariants that hold for
//! any workload: replaying the change log rebuilds the same state, and a
//! key never has two live lease holders.
//!
//! Slow-ish by design, so not run in CI: `scripts/storage-tests.sh`.
//! `PROPTEST_CASES=2000 cargo test -p tab-store --test properties` digs deeper.

use chrono::{DateTime, Duration, Utc};
use proptest::prelude::*;
use serde_json::json;
use tab_domain::lease::Claim;
use tab_domain::ops::Op;
use tab_store::HubStore;
use tab_store::mem::MemStore;
use tab_store::sqlite::SqliteStore;

const KINDS: &[&str] = &["item", "note"];
const IDS: &[&str] = &["1", "2", "3", "a", "b"];
const NODES: &[&str] = &[
    "item:1",
    "item:2",
    "item:a",
    "note:1",
    "project:x",
    "person:p",
];
const RELS: &[&str] = &["in", "blocks", "mentions"];
const WORDS: &[&str] = &[
    "login",
    "Login",
    "fix",
    "page",
    "ui",
    "école",
    "日本",
    "a b",
    "\"q\"",
    // File references (sha256 of "a" and "b").
    "sha256:ca978112ca1bbdcafac231b39a23dc4da786eff8147c4e72b9807785afee48bb",
    "sha256:3e23e8160039594a33894f6564e1b1348bbd7a0088d42c4acb73eeaed59c009d",
];
const QUERIES: &[&str] = &[
    "login", "LOG", "fix page", "ui", "é", "日本", "\"q", "b", "zzz",
];
const HOLDERS: &[&str] = &["mac", "phone", "runner"];
const KEYS: &[&str] = &["k1", "k2", "k3"];

fn t(s: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(1_800_000_000 + s, 0).unwrap()
}

fn pick<T: Clone + std::fmt::Debug + 'static>(xs: &'static [T]) -> impl Strategy<Value = T> {
    prop::sample::select(xs)
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        3 => (pick(KINDS), pick(IDS), prop::collection::vec(pick(WORDS), 0..3)).prop_map(
            |(k, i, words)| Op::Put {
                kind: k.into(),
                id: i.into(),
                body: json!({ "title": words.join(" "), "tags": words }),
            }
        ),
        1 => (pick(KINDS), pick(IDS)).prop_map(|(k, i)| Op::Delete {
            kind: k.into(),
            id: i.into()
        }),
        2 => (pick(NODES), pick(NODES), pick(RELS)).prop_map(|(f, t, r)| Op::Link {
            from: f.into(),
            to: t.into(),
            rel: r.into()
        }),
        1 => (pick(NODES), pick(NODES), pick(RELS)).prop_map(|(f, t, r)| Op::Unlink {
            from: f.into(),
            to: t.into(),
            rel: r.into()
        }),
        // Now and then an invalid op, which must reject its whole batch.
        1 => Just(Op::Put { kind: "Bad".into(), id: "x".into(), body: json!({}) }),
    ]
}

fn batches() -> impl Strategy<Value = Vec<Vec<Op>>> {
    prop::collection::vec(prop::collection::vec(op(), 0..6), 1..20)
}

/// Everything a client can observe, in one comparable value.
fn observe<S: HubStore>(s: &S) -> String {
    let mut out = format!("head {}\n", s.head().unwrap());
    for a in s.changes_since(0, 10_000).unwrap() {
        out += &format!("log {} {:?} {} {}\n", a.seq, a.op, a.by, a.at);
    }
    for k in KINDS {
        for i in IDS {
            out += &format!("get {k}:{i} {:?}\n", s.get(k, i).unwrap());
        }
    }
    for n in NODES {
        out += &format!("nb {n} {:?}\n", s.graph_neighbors(n, 100).unwrap());
    }
    for q in QUERIES {
        out += &format!("search {q} {:?}\n", s.search(q, 100).unwrap());
    }
    out += &format!("files {:?}\n", s.blob_refs().unwrap());
    out
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// SQLite behaves exactly like the in-memory reference.
    #[test]
    fn backends_agree(batches in batches()) {
        let mut mem = MemStore::new();
        let mut lite = SqliteStore::in_memory().unwrap();
        for (i, batch) in batches.iter().enumerate() {
            let a = mem.apply("dev", batch, t(i as i64)).map_err(|e| e.to_string());
            let b = lite.apply("dev", batch, t(i as i64)).map_err(|e| e.to_string());
            prop_assert_eq!(a, b);
            prop_assert_eq!(observe(&mem), observe(&lite));
        }
    }

    /// Replaying a store's log into an empty one rebuilds the same state,
    /// in either backend. This is what sync and migration rely on.
    #[test]
    fn replaying_the_log_converges(batches in batches()) {
        let mut src = SqliteStore::in_memory().unwrap();
        for (i, batch) in batches.iter().enumerate() {
            let _ = src.apply("dev", batch, t(i as i64));
        }
        let mut mem = MemStore::new();
        let mut lite = SqliteStore::in_memory().unwrap();
        for a in src.changes_since(0, 10_000).unwrap() {
            mem.apply(&a.by, std::slice::from_ref(&a.op), a.at).unwrap();
            lite.apply(&a.by, std::slice::from_ref(&a.op), a.at).unwrap();
        }
        prop_assert_eq!(observe(&src), observe(&mem));
        prop_assert_eq!(observe(&src), observe(&lite));
    }
}

#[derive(Debug, Clone)]
enum LeaseStep {
    Claim(&'static str, &'static str, i64),
    Release(&'static str, &'static str),
    Expire,
    Wait(i64),
}

fn lease_step() -> impl Strategy<Value = LeaseStep> {
    prop_oneof![
        4 => (pick(KEYS), pick(HOLDERS), 1i64..60).prop_map(|(k, h, ttl)| LeaseStep::Claim(k, h, ttl)),
        2 => (pick(KEYS), pick(HOLDERS)).prop_map(|(k, h)| LeaseStep::Release(k, h)),
        1 => Just(LeaseStep::Expire),
        2 => (0i64..40).prop_map(LeaseStep::Wait),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Backends agree on every lease outcome; a key never has two live
    /// holders; tokens only go up per key.
    #[test]
    fn lease_invariants(steps in prop::collection::vec(lease_step(), 1..60)) {
        let mut mem = MemStore::new();
        let mut lite = SqliteStore::in_memory().unwrap();
        let mut now = 0;
        let mut holder: std::collections::HashMap<&str, (String, u64, DateTime<Utc>)> = Default::default();
        let mut top: std::collections::HashMap<&str, u64> = Default::default();
        for step in steps {
            match step {
                LeaseStep::Claim(k, h, ttl) => {
                    let a = mem.claim_lease(k, h, Duration::seconds(ttl), t(now)).unwrap();
                    let b = lite.claim_lease(k, h, Duration::seconds(ttl), t(now)).unwrap();
                    prop_assert_eq!(&a, &b);
                    let l = a.lease();
                    let live = holder.get(k).filter(|(_, _, until)| t(now) < *until);
                    match (&a, live) {
                        (Claim::Held { .. }, Some((who, tok, _))) => {
                            prop_assert_ne!(who.as_str(), h);
                            prop_assert_eq!(&l.holder, who);
                            prop_assert_eq!(l.token, *tok);
                        }
                        (Claim::Renewed { .. }, Some((who, tok, _))) => {
                            prop_assert_eq!(who.as_str(), h);
                            prop_assert_eq!(l.token, *tok);
                        }
                        (Claim::Granted { .. }, None) => {
                            prop_assert!(l.token > top.get(k).copied().unwrap_or(0));
                        }
                        (outcome, live) => prop_assert!(false, "{outcome:?} while live = {live:?}"),
                    }
                    if a.won() {
                        top.insert(k, l.token);
                        holder.insert(k, (l.holder.clone(), l.token, l.expires_at));
                    }
                }
                LeaseStep::Release(k, h) => {
                    let a = mem.release_lease(k, h, t(now)).unwrap();
                    prop_assert_eq!(a, lite.release_lease(k, h, t(now)).unwrap());
                    let expected = holder
                        .get(k)
                        .is_some_and(|(who, _, until)| who == h && t(now) < *until);
                    prop_assert_eq!(a, expected);
                    if a {
                        holder.remove(k);
                    }
                }
                LeaseStep::Expire => {
                    let a = mem.expire_leases(t(now)).unwrap();
                    prop_assert_eq!(&a, &lite.expire_leases(t(now)).unwrap());
                    for l in &a {
                        prop_assert!(l.expires_at <= t(now));
                        holder.remove(l.key.as_str());
                    }
                }
                LeaseStep::Wait(s) => now += s,
            }
        }
    }
}

// ---- File stores ----

#[derive(Debug, Clone)]
enum BlobStep {
    Put(u8, i64),
    Delete(u8),
}

fn blob_step() -> impl Strategy<Value = BlobStep> {
    prop_oneof![
        3 => (0u8..8, 0i64..100).prop_map(|(f, at)| BlobStep::Put(f, at)),
        1 => (0u8..8).prop_map(BlobStep::Delete),
    ]
}

fn file(f: u8) -> Vec<u8> {
    vec![f; f as usize * 37]
}

fn observe_blobs<B: tab_store::blob::BlobStore>(b: &B) -> String {
    let mut out = format!("stats {:?}\n", b.stats().unwrap());
    for m in b.list("", 1000).unwrap() {
        out += &format!("{} {} {}\n", m.hash, m.size, m.stored_at);
    }
    for f in 0..8 {
        let h = tab_domain::blob::hash(&file(f));
        out += &format!("get {f} {:?}\n", b.get(&h).unwrap().map(|v| v.len()));
    }
    out
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// The folder store behaves exactly like the in-memory reference.
    #[test]
    fn file_stores_agree(steps in prop::collection::vec(blob_step(), 1..40)) {
        use tab_store::blob::{BlobStore, MemBlobStore};
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!(
            "tab-blob-props/{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let mut mem = MemBlobStore::new();
        let mut fs = tab_store::fs_blob::FsBlobStore::open(&dir).unwrap();
        for step in steps {
            match step {
                BlobStep::Put(f, at) => {
                    let a = mem.put(&file(f), t(at)).map_err(|e| e.to_string());
                    let b = fs.put(&file(f), t(at)).map_err(|e| e.to_string());
                    prop_assert_eq!(a, b);
                }
                BlobStep::Delete(f) => {
                    let h = tab_domain::blob::hash(&file(f));
                    prop_assert_eq!(mem.delete(&h).unwrap(), fs.delete(&h).unwrap());
                }
            }
            prop_assert_eq!(observe_blobs(&mem), observe_blobs(&fs));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
