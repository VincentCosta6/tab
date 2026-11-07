//! The behavior every [`BlobStore`] backend must have.
//!
//! Same rules as [`crate::conformance`]: every behavior has a check here,
//! every backend runs all of them through [`blob_conformance_tests!`](crate::blob_conformance_tests),
//! and a test fails if a `pub fn` check isn't registered in the macro.

use chrono::{DateTime, Utc};
use tab_domain::blob::{MAX_BLOB_BYTES, hash};

use crate::StoreError;
use crate::blob::{BlobStats, BlobStore};

/// Makes one `#[test]` per blob check, each on a fresh store from `$make`.
#[macro_export]
macro_rules! blob_conformance_tests {
    ($make:expr) => {
        $crate::blob_conformance_tests!(@each $make;
            missing_files_are_none,
            put_returns_hash_size_and_time,
            put_then_get_round_trips,
            empty_files_are_fine,
            binary_bytes_round_trip,
            putting_the_same_bytes_again_keeps_one_copy,
            putting_again_keeps_the_first_stored_at,
            different_bytes_are_different_files,
            oversized_files_are_refused,
            a_file_at_the_limit_is_taken,
            delete_removes_the_file,
            delete_missing_is_false,
            put_after_delete_stores_again,
            malformed_hashes_find_nothing,
            list_is_in_hash_order,
            list_pages_with_after,
            list_respects_limit,
            list_limit_zero_returns_nothing,
            list_after_the_last_is_empty,
            stats_count_files_and_bytes,
            stats_of_an_empty_store_are_zero
        );
    };
    (@each $make:expr; $($name:ident),* $(,)?) => {
        /// Every check name, so a test can compare it with the module.
        #[allow(dead_code)]
        pub const BLOB_CONFORMANCE_CHECKS: &[&str] = &[$(stringify!($name)),*];
        $(
            #[test]
            fn $name() {
                let mut store = $make;
                $crate::blob_conformance::$name(&mut store);
            }
        )*
    };
}

/// Names of the checks defined in this file, for the registry test.
pub fn defined_checks() -> Vec<&'static str> {
    include_str!("blob_conformance.rs")
        .lines()
        .filter_map(|l| l.strip_prefix("pub fn "))
        .filter_map(|l| l.split(['<', '(']).next())
        .filter(|n| *n != "defined_checks")
        .collect()
}

// Whole seconds: a folder backend keeps times as file modification times.
fn t(s: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(1_800_000_000 + s, 0).unwrap()
}

fn hashes<B: BlobStore>(b: &B, after: &str, limit: usize) -> Vec<String> {
    b.list(after, limit)
        .unwrap()
        .into_iter()
        .map(|m| m.hash)
        .collect()
}

fn sorted_hashes(data: &[&[u8]]) -> Vec<String> {
    let mut h: Vec<String> = data.iter().map(|d| hash(d)).collect();
    h.sort();
    h
}

pub fn missing_files_are_none<B: BlobStore>(b: &mut B) {
    let h = hash(b"never stored");
    assert_eq!(b.get(&h).unwrap(), None);
    assert_eq!(b.stat(&h).unwrap(), None);
}

pub fn put_returns_hash_size_and_time<B: BlobStore>(b: &mut B) {
    let m = b.put(b"hello", t(1)).unwrap();
    assert_eq!(m.hash, hash(b"hello"));
    assert_eq!(m.size, 5);
    assert_eq!(m.stored_at, t(1));
    assert_eq!(b.stat(&m.hash).unwrap(), Some(m));
}

pub fn put_then_get_round_trips<B: BlobStore>(b: &mut B) {
    let m = b.put(b"a screenshot", t(1)).unwrap();
    assert_eq!(
        b.get(&m.hash).unwrap().as_deref(),
        Some(&b"a screenshot"[..])
    );
}

pub fn empty_files_are_fine<B: BlobStore>(b: &mut B) {
    let m = b.put(b"", t(1)).unwrap();
    assert_eq!(m.size, 0);
    assert_eq!(b.get(&m.hash).unwrap(), Some(vec![]));
}

pub fn binary_bytes_round_trip<B: BlobStore>(b: &mut B) {
    let bytes: Vec<u8> = (0..=255u8).cycle().take(70_000).collect();
    let m = b.put(&bytes, t(1)).unwrap();
    assert_eq!(m.size, 70_000);
    assert_eq!(b.get(&m.hash).unwrap(), Some(bytes));
}

pub fn putting_the_same_bytes_again_keeps_one_copy<B: BlobStore>(b: &mut B) {
    let first = b.put(b"same", t(1)).unwrap();
    let again = b.put(b"same", t(1)).unwrap();
    assert_eq!(first, again);
    assert_eq!(b.stats().unwrap(), BlobStats { files: 1, bytes: 4 });
}

pub fn putting_again_keeps_the_first_stored_at<B: BlobStore>(b: &mut B) {
    b.put(b"same", t(1)).unwrap();
    let again = b.put(b"same", t(500)).unwrap();
    assert_eq!(again.stored_at, t(1));
    assert_eq!(b.stat(&hash(b"same")).unwrap().unwrap().stored_at, t(1));
}

pub fn different_bytes_are_different_files<B: BlobStore>(b: &mut B) {
    let x = b.put(b"x", t(1)).unwrap();
    let y = b.put(b"y", t(2)).unwrap();
    assert_ne!(x.hash, y.hash);
    assert_eq!(b.get(&x.hash).unwrap(), Some(b"x".to_vec()));
    assert_eq!(b.get(&y.hash).unwrap(), Some(b"y".to_vec()));
}

pub fn oversized_files_are_refused<B: BlobStore>(b: &mut B) {
    let big = vec![7u8; MAX_BLOB_BYTES + 1];
    assert!(matches!(b.put(&big, t(1)), Err(StoreError::Invalid(_))));
    assert_eq!(b.stat(&hash(&big)).unwrap(), None);
    assert_eq!(b.stats().unwrap(), BlobStats::default());
}

pub fn a_file_at_the_limit_is_taken<B: BlobStore>(b: &mut B) {
    let max = vec![7u8; MAX_BLOB_BYTES];
    let m = b.put(&max, t(1)).unwrap();
    assert_eq!(m.size, MAX_BLOB_BYTES as u64);
}

pub fn delete_removes_the_file<B: BlobStore>(b: &mut B) {
    let m = b.put(b"gone soon", t(1)).unwrap();
    let keep = b.put(b"stays", t(1)).unwrap();
    assert!(b.delete(&m.hash).unwrap());
    assert_eq!(b.get(&m.hash).unwrap(), None);
    assert_eq!(b.stat(&m.hash).unwrap(), None);
    assert_eq!(hashes(b, "", 10), vec![keep.hash]);
}

pub fn delete_missing_is_false<B: BlobStore>(b: &mut B) {
    assert!(!b.delete(&hash(b"never")).unwrap());
    assert!(!b.delete("not a hash").unwrap());
}

pub fn put_after_delete_stores_again<B: BlobStore>(b: &mut B) {
    let m = b.put(b"back", t(1)).unwrap();
    b.delete(&m.hash).unwrap();
    let again = b.put(b"back", t(9)).unwrap();
    assert_eq!(again.stored_at, t(9));
    assert_eq!(b.get(&m.hash).unwrap(), Some(b"back".to_vec()));
}

pub fn malformed_hashes_find_nothing<B: BlobStore>(b: &mut B) {
    let m = b.put(b"x", t(1)).unwrap();
    for bad in [
        "",
        "x",
        "../../etc/passwd",
        &m.hash.to_uppercase(),
        &m.hash.replace("sha256:", ""),
        &m.hash[..m.hash.len() - 1],
    ] {
        assert_eq!(b.get(bad).unwrap(), None, "{bad}");
        assert_eq!(b.stat(bad).unwrap(), None, "{bad}");
        assert!(!b.delete(bad).unwrap(), "{bad}");
    }
    assert!(b.stat(&m.hash).unwrap().is_some());
}

pub fn list_is_in_hash_order<B: BlobStore>(b: &mut B) {
    let data: &[&[u8]] = &[b"one", b"two", b"three", b"four", b"five"];
    for (i, d) in data.iter().enumerate() {
        b.put(d, t(i as i64)).unwrap();
    }
    assert_eq!(hashes(b, "", 100), sorted_hashes(data));
    let metas = b.list("", 100).unwrap();
    let one = metas.iter().find(|m| m.hash == hash(b"three")).unwrap();
    assert_eq!((one.size, one.stored_at), (5, t(2)));
}

pub fn list_pages_with_after<B: BlobStore>(b: &mut B) {
    let data: Vec<Vec<u8>> = (0..25).map(|i| format!("file {i}").into_bytes()).collect();
    for d in &data {
        b.put(d, t(1)).unwrap();
    }
    let refs: Vec<&[u8]> = data.iter().map(|d| d.as_slice()).collect();
    let want = sorted_hashes(&refs);
    let mut got = vec![];
    let mut after = String::new();
    loop {
        let page = hashes(b, &after, 7);
        assert!(page.len() <= 7);
        let Some(last) = page.last() else { break };
        after = last.clone();
        got.extend(page);
    }
    assert_eq!(got, want);
    // `after` needn't be a stored file.
    let mid = &want[12];
    b.delete(mid).unwrap();
    assert_eq!(hashes(b, mid, 1), vec![want[13].clone()]);
}

pub fn list_respects_limit<B: BlobStore>(b: &mut B) {
    for i in 0..5 {
        b.put(format!("{i}").as_bytes(), t(1)).unwrap();
    }
    assert_eq!(b.list("", 3).unwrap().len(), 3);
    assert_eq!(b.list("", 1).unwrap().len(), 1);
}

pub fn list_limit_zero_returns_nothing<B: BlobStore>(b: &mut B) {
    b.put(b"x", t(1)).unwrap();
    assert!(b.list("", 0).unwrap().is_empty());
}

pub fn list_after_the_last_is_empty<B: BlobStore>(b: &mut B) {
    let x = b.put(b"x", t(1)).unwrap();
    let y = b.put(b"y", t(1)).unwrap();
    let last = x.hash.max(y.hash);
    assert!(b.list(&last, 10).unwrap().is_empty());
    let end = format!("sha256:{}", "f".repeat(64));
    assert!(b.list(&end, 10).unwrap().is_empty());
}

pub fn stats_count_files_and_bytes<B: BlobStore>(b: &mut B) {
    b.put(b"abc", t(1)).unwrap();
    b.put(b"defgh", t(1)).unwrap();
    b.put(b"abc", t(2)).unwrap();
    assert_eq!(b.stats().unwrap(), BlobStats { files: 2, bytes: 8 });
    b.delete(&hash(b"abc")).unwrap();
    assert_eq!(b.stats().unwrap(), BlobStats { files: 1, bytes: 5 });
}

pub fn stats_of_an_empty_store_are_zero<B: BlobStore>(b: &mut B) {
    assert_eq!(b.stats().unwrap(), BlobStats::default());
    assert!(b.list("", 10).unwrap().is_empty());
}
