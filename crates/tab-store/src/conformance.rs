//! The behavior every [`HubStore`] backend must have.
//!
//! **Rule:** every behavior of the storage layer has a check here, and every
//! backend runs all of them. A new or changed trait method lands with its
//! checks; a storage bug gets a failing check before the fix. That's what
//! lets us swap or add a backend (Durable Object SQLite, Postgres) and know
//! it behaves the same.
//!
//! Each check is a `pub fn name<S: HubStore>(s: &mut S)` taking an empty
//! store. Backends run them all with [`conformance_tests!`](crate::conformance_tests),
//! which makes one `#[test]` per check so a failure names the behavior:
//!
//! ```ignore
//! mod sqlite_in_memory {
//!     tab_store::conformance_tests!(tab_store::sqlite::SqliteStore::in_memory().unwrap());
//! }
//! ```
//!
//! Add a check by writing the function and adding its name to the list in
//! [`conformance_tests!`](crate::conformance_tests); a test fails if the
//! two disagree.

use chrono::{DateTime, Duration, Utc};
use serde_json::{Value, json};
use tab_domain::lease::{Claim, MAX_TTL, MIN_TTL};
use tab_domain::ops::{Edge, MAX_BATCH, MAX_BODY_BYTES, Op, SearchHit};

use crate::{HubStore, StoreError, StoreStats};

/// Makes one `#[test]` per conformance check, each on a fresh store from
/// `$make` (an expression evaluated once per test).
#[macro_export]
macro_rules! conformance_tests {
    ($make:expr) => {
        $crate::conformance_tests!(@each $make;
            // Change log
            empty_store_has_head_zero_and_no_changes,
            apply_numbers_ops_consecutively_after_head,
            apply_returns_author_time_and_ops,
            empty_batch_writes_nothing,
            changes_read_back_exactly_what_apply_returned,
            changes_since_is_exclusive_and_ordered,
            changes_since_pages_with_limit,
            changes_since_limit_zero_returns_nothing,
            changes_since_past_head_is_empty,
            head_tracks_many_batches,
            no_op_changes_still_get_logged,
            nanosecond_times_round_trip,
            // Validation
            invalid_batch_writes_nothing,
            invalid_batch_names_the_bad_op,
            bad_kinds_are_rejected,
            bad_ids_and_names_are_rejected,
            non_object_bodies_are_rejected,
            oversized_batches_are_rejected,
            oversized_bodies_are_rejected,
            rejected_batch_does_not_use_up_seqs,
            // Records
            get_missing_is_none,
            put_then_get_returns_body_seq_and_time,
            put_overwrites_and_moves_seq,
            same_id_in_different_kinds_is_separate,
            delete_removes_the_record,
            delete_missing_is_fine,
            put_after_delete_recreates,
            later_ops_in_a_batch_win,
            bodies_round_trip_exactly,
            unusual_ids_round_trip,
            // Graph
            neighbors_of_unknown_node_is_empty,
            link_is_idempotent,
            neighbors_cover_both_directions_sorted,
            neighbors_respect_limit,
            same_pair_with_different_rels_are_separate_edges,
            unlink_removes_only_that_edge,
            unlink_missing_is_fine,
            delete_removes_edges_in_both_directions,
            delete_keeps_unrelated_edges,
            edges_do_not_need_records,
            self_loops_are_listed_once,
            neighbors_sort_in_byte_order,
            relink_after_unlink_works,
            // Search
            empty_query_finds_nothing,
            a_term_matches_inside_words,
            every_term_must_match,
            search_ignores_case,
            short_terms_match,
            newest_change_comes_first,
            search_respects_limit,
            search_limit_zero_returns_nothing,
            search_reads_nested_strings_and_arrays,
            keys_and_numbers_are_not_searched,
            terms_do_not_match_across_separate_strings,
            query_syntax_is_literal,
            rewrites_reindex,
            deletes_unindex,
            unicode_search,
            hits_carry_kind_and_current_seq,
            repeated_terms_are_fine,
            // Leases
            claiming_a_free_key_grants_token_one,
            holder_reclaiming_renews_same_token,
            another_holder_is_refused,
            a_refused_claim_changes_nothing,
            a_lapsed_lease_is_taken_over_with_the_next_token,
            a_lease_lapses_exactly_at_expiry,
            ttl_is_clamped,
            holder_can_release,
            others_cannot_release,
            lapsed_leases_cannot_be_released,
            releasing_unknown_key_is_false,
            tokens_keep_climbing_after_release,
            same_holder_after_lapse_gets_a_new_token,
            expire_returns_lapsed_sorted_and_removes_them,
            expire_leaves_live_leases,
            expire_is_idempotent,
            leases_are_per_key,
            leases_do_not_touch_the_log,
            stale_tokens_lose_after_takeover,
            // Stats
            stats_of_an_empty_store_are_zero,
            stats_count_records_and_edges,
            stats_count_only_live_leases,
            stats_bytes_grow_with_data,
            stats_do_not_count_the_log_as_records
        );
    };
    (@each $make:expr; $($name:ident),* $(,)?) => {
        /// Every check name, so a test can compare it with the module.
        #[allow(dead_code)]
        pub const CONFORMANCE_CHECKS: &[&str] = &[$(stringify!($name)),*];
        $(
            #[test]
            fn $name() {
                let mut store = $make;
                $crate::conformance::$name(&mut store);
            }
        )*
    };
}

/// Names of the checks defined in this file, for the registry test.
pub fn defined_checks() -> Vec<&'static str> {
    include_str!("conformance.rs")
        .lines()
        .filter_map(|l| l.strip_prefix("pub fn "))
        .filter_map(|l| l.split(['<', '(']).next())
        .filter(|n| *n != "defined_checks")
        .collect()
}

// ---- helpers ----

fn t(s: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(1_800_000_000 + s, 123_456_789).unwrap()
}

fn put(kind: &str, id: &str, body: Value) -> Op {
    Op::Put {
        kind: kind.into(),
        id: id.into(),
        body,
    }
}

fn item(id: &str, title: &str) -> Op {
    put("item", id, json!({ "title": title }))
}

fn del(kind: &str, id: &str) -> Op {
    Op::Delete {
        kind: kind.into(),
        id: id.into(),
    }
}

fn link(from: &str, to: &str, rel: &str) -> Op {
    Op::Link {
        from: from.into(),
        to: to.into(),
        rel: rel.into(),
    }
}

fn unlink(from: &str, to: &str, rel: &str) -> Op {
    Op::Unlink {
        from: from.into(),
        to: to.into(),
        rel: rel.into(),
    }
}

fn edge(from: &str, to: &str, rel: &str) -> Edge {
    Edge {
        from: from.into(),
        to: to.into(),
        rel: rel.into(),
    }
}

fn apply<S: HubStore>(s: &mut S, ops: &[Op], at: DateTime<Utc>) {
    s.apply("mac", ops, at).unwrap();
}

fn ids(hits: Vec<SearchHit>) -> Vec<String> {
    hits.into_iter().map(|h| h.id).collect()
}

fn find<S: HubStore>(s: &S, q: &str) -> Vec<String> {
    ids(s.search(q, 100).unwrap())
}

fn rejected<S: HubStore>(s: &mut S, ops: &[Op]) -> usize {
    match s.apply("mac", ops, t(0)) {
        Err(StoreError::Invalid(e)) => e.index,
        other => panic!("expected the batch to be rejected, got {other:?}"),
    }
}

fn granted(c: &Claim) -> bool {
    matches!(c, Claim::Granted { .. })
}

// ---- change log ----

pub fn empty_store_has_head_zero_and_no_changes<S: HubStore>(s: &mut S) {
    assert_eq!(s.head().unwrap(), 0);
    assert!(s.changes_since(0, 100).unwrap().is_empty());
}

pub fn apply_numbers_ops_consecutively_after_head<S: HubStore>(s: &mut S) {
    let a = s
        .apply(
            "mac",
            &[item("1", "a"), item("2", "b"), item("3", "c")],
            t(0),
        )
        .unwrap();
    assert_eq!(a.iter().map(|a| a.seq).collect::<Vec<_>>(), [1, 2, 3]);
    let b = s.apply("mac", &[item("4", "d")], t(1)).unwrap();
    assert_eq!(b[0].seq, 4);
}

pub fn apply_returns_author_time_and_ops<S: HubStore>(s: &mut S) {
    let ops = [item("1", "a"), link("item:1", "x:y", "r")];
    let a = s.apply("phone", &ops, t(7)).unwrap();
    assert_eq!(a.len(), 2);
    for (applied, op) in a.iter().zip(&ops) {
        assert_eq!(applied.by, "phone");
        assert_eq!(applied.at, t(7));
        assert_eq!(&applied.op, op);
    }
}

pub fn empty_batch_writes_nothing<S: HubStore>(s: &mut S) {
    assert!(s.apply("mac", &[], t(0)).unwrap().is_empty());
    assert_eq!(s.head().unwrap(), 0);
    apply(s, &[item("1", "a")], t(0));
    assert!(s.apply("mac", &[], t(1)).unwrap().is_empty());
    assert_eq!(s.head().unwrap(), 1);
}

pub fn changes_read_back_exactly_what_apply_returned<S: HubStore>(s: &mut S) {
    let mut written = s
        .apply(
            "mac",
            &[
                put(
                    "item",
                    "1",
                    json!({"title": "Ünïcødé ✓", "n": 1.5, "deep": {"a": [1, null, true]}}),
                ),
                link("item:1", "project:tab", "in"),
            ],
            t(0),
        )
        .unwrap();
    written.extend(
        s.apply(
            "phone",
            &[unlink("item:1", "project:tab", "in"), del("item", "1")],
            t(1),
        )
        .unwrap(),
    );
    assert_eq!(s.changes_since(0, 100).unwrap(), written);
}

pub fn changes_since_is_exclusive_and_ordered<S: HubStore>(s: &mut S) {
    for i in 0..5 {
        apply(s, &[item(&i.to_string(), "x")], t(i));
    }
    let seqs: Vec<_> = s
        .changes_since(2, 100)
        .unwrap()
        .iter()
        .map(|a| a.seq)
        .collect();
    assert_eq!(seqs, [3, 4, 5]);
}

pub fn changes_since_pages_with_limit<S: HubStore>(s: &mut S) {
    let ops: Vec<_> = (0..10).map(|i| item(&i.to_string(), "x")).collect();
    apply(s, &ops, t(0));
    let mut since = 0;
    let mut pages = vec![];
    loop {
        let page = s.changes_since(since, 3).unwrap();
        let Some(last) = page.last() else { break };
        since = last.seq;
        pages.push(page.iter().map(|a| a.seq).collect::<Vec<_>>());
    }
    assert_eq!(
        pages,
        [vec![1, 2, 3], vec![4, 5, 6], vec![7, 8, 9], vec![10]]
    );
}

pub fn changes_since_limit_zero_returns_nothing<S: HubStore>(s: &mut S) {
    apply(s, &[item("1", "a")], t(0));
    assert!(s.changes_since(0, 0).unwrap().is_empty());
}

pub fn changes_since_past_head_is_empty<S: HubStore>(s: &mut S) {
    apply(s, &[item("1", "a")], t(0));
    assert!(s.changes_since(1, 10).unwrap().is_empty());
    assert!(s.changes_since(1_000, 10).unwrap().is_empty());
    assert!(s.changes_since(u64::MAX >> 2, 10).unwrap().is_empty());
}

pub fn head_tracks_many_batches<S: HubStore>(s: &mut S) {
    let mut expected = 0;
    for n in 1..=20 {
        let ops: Vec<_> = (0..n).map(|i| item(&format!("{n}-{i}"), "x")).collect();
        apply(s, &ops, t(n as i64));
        expected += n as u64;
        assert_eq!(s.head().unwrap(), expected);
    }
}

pub fn no_op_changes_still_get_logged<S: HubStore>(s: &mut S) {
    // Deleting nothing, linking twice, unlinking nothing: the log keeps
    // what was asked, so replaying it anywhere ends the same way.
    apply(s, &[link("a", "b", "r")], t(0));
    let a = s
        .apply(
            "mac",
            &[
                del("item", "ghost"),
                link("a", "b", "r"),
                unlink("x", "y", "r"),
            ],
            t(1),
        )
        .unwrap();
    assert_eq!(a.len(), 3);
    assert_eq!(s.head().unwrap(), 4);
    assert_eq!(s.changes_since(1, 10).unwrap(), a);
}

pub fn nanosecond_times_round_trip<S: HubStore>(s: &mut S) {
    let times = [
        DateTime::from_timestamp(0, 0).unwrap(),
        DateTime::from_timestamp(1, 1).unwrap(),
        DateTime::from_timestamp(1_800_000_000, 999_999_999).unwrap(),
        DateTime::from_timestamp(4_102_444_800, 500).unwrap(), // 2100
    ];
    for (i, at) in times.iter().enumerate() {
        apply(s, &[item(&i.to_string(), "x")], *at);
    }
    let back: Vec<_> = s
        .changes_since(0, 10)
        .unwrap()
        .iter()
        .map(|a| a.at)
        .collect();
    assert_eq!(back, times);
    assert_eq!(s.get("item", "3").unwrap().unwrap().updated_at, times[3]);
}

// ---- validation ----

pub fn invalid_batch_writes_nothing<S: HubStore>(s: &mut S) {
    apply(s, &[item("keep", "x"), link("a", "b", "r")], t(0));
    rejected(
        s,
        &[
            item("new", "x"),
            del("item", "keep"),
            unlink("a", "b", "r"),
            item("", "bad"),
        ],
    );
    assert_eq!(s.head().unwrap(), 2);
    assert!(s.get("item", "new").unwrap().is_none());
    assert!(s.get("item", "keep").unwrap().is_some());
    assert_eq!(s.graph_neighbors("a", 10).unwrap(), [edge("a", "b", "r")]);
    assert!(find(s, "bad").is_empty());
}

pub fn invalid_batch_names_the_bad_op<S: HubStore>(s: &mut S) {
    assert_eq!(rejected(s, &[item("", "x")]), 0);
    assert_eq!(
        rejected(s, &[item("1", "x"), item("2", "x"), del("", "3")]),
        2
    );
}

pub fn bad_kinds_are_rejected<S: HubStore>(s: &mut S) {
    for kind in [
        "",
        "Item",
        "my item",
        "item-x",
        "item:x",
        "ïtem",
        &"k".repeat(257),
    ] {
        rejected(s, &[put(kind, "1", json!({}))]);
        rejected(s, &[del(kind, "1")]);
    }
    // These are fine.
    for kind in ["item", "item_2", "gh.pr"] {
        apply(s, &[put(kind, "1", json!({}))], t(0));
    }
}

pub fn bad_ids_and_names_are_rejected<S: HubStore>(s: &mut S) {
    let long = "x".repeat(257);
    for bad in ["", "tab\there", "new\nline", "nul\0", long.as_str()] {
        rejected(s, &[put("item", bad, json!({}))]);
        rejected(s, &[link(bad, "b", "r")]);
        rejected(s, &[link("a", bad, "r")]);
        rejected(s, &[link("a", "b", bad)]);
        rejected(s, &[unlink(bad, "b", "r")]);
    }
    apply(s, &[put("item", &"x".repeat(256), json!({}))], t(0));
}

pub fn non_object_bodies_are_rejected<S: HubStore>(s: &mut S) {
    for body in [
        json!(null),
        json!("text"),
        json!(1),
        json!([1, 2]),
        json!(true),
    ] {
        rejected(s, &[put("item", "1", body)]);
    }
    apply(s, &[put("item", "1", json!({}))], t(0));
}

pub fn oversized_batches_are_rejected<S: HubStore>(s: &mut S) {
    let ops: Vec<_> = (0..=MAX_BATCH).map(|i| item(&i.to_string(), "x")).collect();
    rejected(s, &ops);
    assert_eq!(s.head().unwrap(), 0);
    apply(s, &ops[..MAX_BATCH], t(0));
    assert_eq!(s.head().unwrap(), MAX_BATCH as u64);
}

pub fn oversized_bodies_are_rejected<S: HubStore>(s: &mut S) {
    let big = "x".repeat(MAX_BODY_BYTES);
    rejected(s, &[put("item", "1", json!({ "t": big }))]);
    let fits = "x".repeat(MAX_BODY_BYTES - 10);
    apply(s, &[put("item", "1", json!({ "t": fits }))], t(0));
    assert_eq!(
        s.get("item", "1").unwrap().unwrap().body["t"]
            .as_str()
            .unwrap()
            .len(),
        MAX_BODY_BYTES - 10
    );
}

pub fn rejected_batch_does_not_use_up_seqs<S: HubStore>(s: &mut S) {
    apply(s, &[item("1", "a")], t(0));
    rejected(s, &[item("2", "b"), item("", "c")]);
    let a = s.apply("mac", &[item("2", "b")], t(1)).unwrap();
    assert_eq!(a[0].seq, 2);
}

// ---- records ----

pub fn get_missing_is_none<S: HubStore>(s: &mut S) {
    assert!(s.get("item", "1").unwrap().is_none());
    apply(s, &[item("1", "a")], t(0));
    assert!(s.get("item", "2").unwrap().is_none());
    assert!(s.get("note", "1").unwrap().is_none());
}

pub fn put_then_get_returns_body_seq_and_time<S: HubStore>(s: &mut S) {
    apply(s, &[item("0", "x"), item("1", "first")], t(3));
    let r = s.get("item", "1").unwrap().unwrap();
    assert_eq!((r.kind.as_str(), r.id.as_str()), ("item", "1"));
    assert_eq!(r.body, json!({"title": "first"}));
    assert_eq!(r.seq, 2);
    assert_eq!(r.updated_at, t(3));
    assert_eq!(r.node(), "item:1");
}

pub fn put_overwrites_and_moves_seq<S: HubStore>(s: &mut S) {
    apply(s, &[put("item", "1", json!({"a": 1, "b": 2}))], t(0));
    apply(s, &[item("2", "other")], t(1));
    apply(s, &[put("item", "1", json!({"a": 3}))], t(2));
    let r = s.get("item", "1").unwrap().unwrap();
    // Replaced, not merged.
    assert_eq!(r.body, json!({"a": 3}));
    assert_eq!(r.seq, 3);
    assert_eq!(r.updated_at, t(2));
    assert_eq!(s.get("item", "2").unwrap().unwrap().seq, 2);
}

pub fn same_id_in_different_kinds_is_separate<S: HubStore>(s: &mut S) {
    apply(
        s,
        &[
            put("item", "1", json!({"k": "item"})),
            put("note", "1", json!({"k": "note"})),
        ],
        t(0),
    );
    apply(s, &[del("item", "1")], t(1));
    assert!(s.get("item", "1").unwrap().is_none());
    assert_eq!(
        s.get("note", "1").unwrap().unwrap().body,
        json!({"k": "note"})
    );
}

pub fn delete_removes_the_record<S: HubStore>(s: &mut S) {
    apply(s, &[item("1", "a"), item("2", "b")], t(0));
    apply(s, &[del("item", "1")], t(1));
    assert!(s.get("item", "1").unwrap().is_none());
    assert!(s.get("item", "2").unwrap().is_some());
}

pub fn delete_missing_is_fine<S: HubStore>(s: &mut S) {
    let a = s.apply("mac", &[del("item", "nope")], t(0)).unwrap();
    assert_eq!(a.len(), 1);
    assert!(s.get("item", "nope").unwrap().is_none());
}

pub fn put_after_delete_recreates<S: HubStore>(s: &mut S) {
    apply(s, &[item("1", "old")], t(0));
    apply(s, &[del("item", "1")], t(1));
    apply(s, &[item("1", "new")], t(2));
    let r = s.get("item", "1").unwrap().unwrap();
    assert_eq!(r.body, json!({"title": "new"}));
    assert_eq!(r.seq, 3);
    assert_eq!(find(s, "new"), ["1"]);
    assert!(find(s, "old").is_empty());
}

pub fn later_ops_in_a_batch_win<S: HubStore>(s: &mut S) {
    apply(s, &[item("1", "first"), item("1", "second")], t(0));
    let r = s.get("item", "1").unwrap().unwrap();
    assert_eq!(r.body, json!({"title": "second"}));
    assert_eq!(r.seq, 2);
    apply(s, &[item("2", "x"), del("item", "2")], t(1));
    assert!(s.get("item", "2").unwrap().is_none());
    apply(s, &[del("item", "3"), item("3", "back")], t(2));
    assert!(s.get("item", "3").unwrap().is_some());
    apply(s, &[link("a", "b", "r"), unlink("a", "b", "r")], t(3));
    assert!(s.graph_neighbors("a", 10).unwrap().is_empty());
}

pub fn bodies_round_trip_exactly<S: HubStore>(s: &mut S) {
    let bodies = [
        json!({}),
        json!({"empty": "", "null": null, "t": true, "f": false}),
        json!({"big": u64::MAX, "neg": i64::MIN, "pi": std::f64::consts::PI, "tiny": 1e-300}),
        json!({"text": "quotes \" and \\ backslashes, tabs\t, newlines\n, nul \u{0}"}),
        json!({"unicode": "日本語 ✓ 🎉 é", "rtl": "שלום"}),
        json!({"deep": {"a": [{"b": [[], {}, [1, [2, [3]]]]}]}}),
        json!({"sql": "'; DROP TABLE hub_records; --"}),
    ];
    for (i, body) in bodies.iter().enumerate() {
        apply(s, &[put("item", &i.to_string(), body.clone())], t(0));
    }
    for (i, body) in bodies.iter().enumerate() {
        assert_eq!(&s.get("item", &i.to_string()).unwrap().unwrap().body, body);
    }
}

pub fn unusual_ids_round_trip<S: HubStore>(s: &mut S) {
    let ids = [
        "github:pr:vincentcosta6/tab#42",
        "a/b/c",
        "with space",
        "quote\"d",
        "it's",
        "emoji 🎉",
        "%_wild*card?",
        "UPPER",
        "upper",
    ];
    for id in ids {
        apply(s, &[put("item", id, json!({ "id": id }))], t(0));
    }
    for id in ids {
        assert_eq!(
            s.get("item", id).unwrap().unwrap().body,
            json!({ "id": id })
        );
    }
    // Wildcards are not patterns.
    assert!(s.get("item", "%").unwrap().is_none());
    assert!(s.get("item", "%_wild*card_").unwrap().is_none());
}

// ---- graph ----

pub fn neighbors_of_unknown_node_is_empty<S: HubStore>(s: &mut S) {
    assert!(s.graph_neighbors("item:1", 10).unwrap().is_empty());
    apply(s, &[link("a", "b", "r")], t(0));
    assert!(s.graph_neighbors("c", 10).unwrap().is_empty());
}

pub fn link_is_idempotent<S: HubStore>(s: &mut S) {
    apply(s, &[link("a", "b", "r"), link("a", "b", "r")], t(0));
    apply(s, &[link("a", "b", "r")], t(1));
    assert_eq!(s.graph_neighbors("a", 10).unwrap(), [edge("a", "b", "r")]);
}

pub fn neighbors_cover_both_directions_sorted<S: HubStore>(s: &mut S) {
    apply(
        s,
        &[
            link("n", "z", "r"),
            link("b", "n", "r"),
            link("n", "a", "r"),
            link("a", "n", "q"),
            link("x", "y", "r"),
        ],
        t(0),
    );
    assert_eq!(
        s.graph_neighbors("n", 10).unwrap(),
        [
            edge("a", "n", "q"),
            edge("b", "n", "r"),
            edge("n", "a", "r"),
            edge("n", "z", "r"),
        ]
    );
}

pub fn neighbors_respect_limit<S: HubStore>(s: &mut S) {
    let ops: Vec<_> = (0..10)
        .map(|i| link("hub", &format!("n{i}"), "r"))
        .collect();
    apply(s, &ops, t(0));
    assert_eq!(s.graph_neighbors("hub", 3).unwrap().len(), 3);
    assert_eq!(
        s.graph_neighbors("hub", 3).unwrap(),
        s.graph_neighbors("hub", 100).unwrap()[..3]
    );
    assert!(s.graph_neighbors("hub", 0).unwrap().is_empty());
    assert_eq!(s.graph_neighbors("hub", 100).unwrap().len(), 10);
}

pub fn same_pair_with_different_rels_are_separate_edges<S: HubStore>(s: &mut S) {
    apply(
        s,
        &[link("a", "b", "blocks"), link("a", "b", "mentions")],
        t(0),
    );
    assert_eq!(s.graph_neighbors("a", 10).unwrap().len(), 2);
    apply(s, &[unlink("a", "b", "blocks")], t(1));
    assert_eq!(
        s.graph_neighbors("a", 10).unwrap(),
        [edge("a", "b", "mentions")]
    );
}

pub fn unlink_removes_only_that_edge<S: HubStore>(s: &mut S) {
    apply(
        s,
        &[
            link("a", "b", "r"),
            link("b", "a", "r"),
            link("a", "c", "r"),
        ],
        t(0),
    );
    apply(s, &[unlink("a", "b", "r")], t(1));
    assert_eq!(
        s.graph_neighbors("a", 10).unwrap(),
        [edge("a", "c", "r"), edge("b", "a", "r")]
    );
}

pub fn unlink_missing_is_fine<S: HubStore>(s: &mut S) {
    apply(s, &[link("a", "b", "r")], t(0));
    apply(s, &[unlink("a", "b", "other"), unlink("b", "a", "r")], t(1));
    assert_eq!(s.graph_neighbors("a", 10).unwrap(), [edge("a", "b", "r")]);
}

pub fn delete_removes_edges_in_both_directions<S: HubStore>(s: &mut S) {
    apply(
        s,
        &[
            item("1", "x"),
            link("item:1", "project:tab", "in"),
            link("person:ana", "item:1", "reviews"),
        ],
        t(0),
    );
    apply(s, &[del("item", "1")], t(1));
    assert!(s.graph_neighbors("item:1", 10).unwrap().is_empty());
    assert!(s.graph_neighbors("project:tab", 10).unwrap().is_empty());
    assert!(s.graph_neighbors("person:ana", 10).unwrap().is_empty());
}

pub fn delete_keeps_unrelated_edges<S: HubStore>(s: &mut S) {
    apply(
        s,
        &[
            item("1", "x"),
            link("item:1", "p", "in"),
            link("item:10", "p", "in"),
            link("item:2", "item:11", "r"),
        ],
        t(0),
    );
    apply(s, &[del("item", "1")], t(1));
    assert_eq!(
        s.graph_neighbors("p", 10).unwrap(),
        [edge("item:10", "p", "in")]
    );
    assert_eq!(s.graph_neighbors("item:2", 10).unwrap().len(), 1);
}

pub fn edges_do_not_need_records<S: HubStore>(s: &mut S) {
    apply(s, &[link("person:ana", "project:tab", "owns")], t(0));
    assert_eq!(s.graph_neighbors("project:tab", 10).unwrap().len(), 1);
    // Deleting a record that never existed still clears its node's edges.
    apply(s, &[del("person", "ana")], t(1));
    assert!(s.graph_neighbors("project:tab", 10).unwrap().is_empty());
}

pub fn self_loops_are_listed_once<S: HubStore>(s: &mut S) {
    apply(s, &[link("a", "a", "self")], t(0));
    assert_eq!(
        s.graph_neighbors("a", 10).unwrap(),
        [edge("a", "a", "self")]
    );
}

pub fn neighbors_sort_in_byte_order<S: HubStore>(s: &mut S) {
    apply(
        s,
        &[
            link("n", "b", "r"),
            link("n", "B", "r"),
            link("n", "é", "r"),
            link("n", "a1", "r"),
            link("n", "a10", "r"),
            link("n", "a2", "r"),
            link("n", "_", "r"),
        ],
        t(0),
    );
    let to: Vec<_> = s
        .graph_neighbors("n", 10)
        .unwrap()
        .into_iter()
        .map(|e| e.to)
        .collect();
    assert_eq!(to, ["B", "_", "a1", "a10", "a2", "b", "é"]);
}

pub fn relink_after_unlink_works<S: HubStore>(s: &mut S) {
    apply(s, &[link("a", "b", "r")], t(0));
    apply(s, &[unlink("a", "b", "r")], t(1));
    apply(s, &[link("a", "b", "r")], t(2));
    assert_eq!(s.graph_neighbors("b", 10).unwrap(), [edge("a", "b", "r")]);
}

// ---- search ----

fn corpus<S: HubStore>(s: &mut S) {
    apply(
        s,
        &[
            put(
                "item",
                "a",
                json!({"title": "Fix Login redirect", "repo": "tab"}),
            ),
            put(
                "item",
                "b",
                json!({"title": "Review login page", "labels": ["ui", "auth"]}),
            ),
            put("note", "c", json!({"text": "lunch at noon"})),
        ],
        t(0),
    );
}

pub fn empty_query_finds_nothing<S: HubStore>(s: &mut S) {
    corpus(s);
    assert!(find(s, "").is_empty());
    assert!(find(s, "   \t\n").is_empty());
}

pub fn a_term_matches_inside_words<S: HubStore>(s: &mut S) {
    corpus(s);
    assert_eq!(find(s, "direc"), ["a"]);
    assert_eq!(find(s, "oon"), ["c"]);
    assert_eq!(find(s, "edirect"), ["a"]);
}

pub fn every_term_must_match<S: HubStore>(s: &mut S) {
    corpus(s);
    assert_eq!(find(s, "login fix"), ["a"]);
    assert_eq!(find(s, "login page"), ["b"]);
    assert!(find(s, "login lunch").is_empty());
}

pub fn search_ignores_case<S: HubStore>(s: &mut S) {
    corpus(s);
    assert_eq!(find(s, "LOGIN"), ["b", "a"]);
    assert_eq!(find(s, "login"), ["b", "a"]);
    assert_eq!(find(s, "LoGiN"), ["b", "a"]);
    assert_eq!(find(s, "fix"), ["a"]);
}

pub fn short_terms_match<S: HubStore>(s: &mut S) {
    corpus(s);
    assert_eq!(find(s, "ui"), ["b"]);
    assert_eq!(find(s, "x"), ["a"]);
    assert_eq!(find(s, "ui auth"), ["b"]);
    assert_eq!(find(s, "lo fix"), ["a"]);
    assert!(find(s, "qq").is_empty());
}

pub fn newest_change_comes_first<S: HubStore>(s: &mut S) {
    apply(s, &[item("1", "shared"), item("2", "shared")], t(0));
    apply(s, &[item("3", "shared")], t(1));
    assert_eq!(find(s, "shared"), ["3", "2", "1"]);
    // Rewriting moves it to the front.
    apply(s, &[item("1", "shared again")], t(2));
    assert_eq!(find(s, "shared"), ["1", "3", "2"]);
}

pub fn search_respects_limit<S: HubStore>(s: &mut S) {
    let ops: Vec<_> = (0..10).map(|i| item(&i.to_string(), "common")).collect();
    apply(s, &ops, t(0));
    assert_eq!(ids(s.search("common", 3).unwrap()), ["9", "8", "7"]);
    assert_eq!(s.search("common", 100).unwrap().len(), 10);
}

pub fn search_limit_zero_returns_nothing<S: HubStore>(s: &mut S) {
    corpus(s);
    assert!(s.search("login", 0).unwrap().is_empty());
}

pub fn search_reads_nested_strings_and_arrays<S: HubStore>(s: &mut S) {
    apply(
        s,
        &[put(
            "item",
            "1",
            json!({"a": {"b": {"c": ["deepword", {"d": "deeper"}]}}}),
        )],
        t(0),
    );
    assert_eq!(find(s, "deepword"), ["1"]);
    assert_eq!(find(s, "deeper"), ["1"]);
}

pub fn keys_and_numbers_are_not_searched<S: HubStore>(s: &mut S) {
    apply(
        s,
        &[put(
            "item",
            "1",
            json!({"title": "x", "count": 12345, "flag": true}),
        )],
        t(0),
    );
    assert!(find(s, "title").is_empty());
    assert!(find(s, "12345").is_empty());
    assert!(find(s, "true").is_empty());
    // Ids and kinds aren't text either.
    apply(s, &[put("item", "zebra", json!({"t": "x"}))], t(1));
    assert!(find(s, "zebra").is_empty());
    assert!(find(s, "item").is_empty());
}

pub fn terms_do_not_match_across_separate_strings<S: HubStore>(s: &mut S) {
    apply(
        s,
        &[put("item", "1", json!({"a": "abc", "b": "def"}))],
        t(0),
    );
    assert!(find(s, "abcdef").is_empty());
    assert!(find(s, "cd").is_empty());
    assert_eq!(find(s, "abc def"), ["1"]);
}

pub fn query_syntax_is_literal<S: HubStore>(s: &mut S) {
    apply(
        s,
        &[
            item("1", "plain words here"),
            item("2", "say \"login\" now"),
            item("3", "a AND b OR c NOT d"),
            item("4", "star* and (parens) and col:on and ^caret -minus"),
            item("5", "100% sure_thing"),
        ],
        t(0),
    );
    assert_eq!(find(s, "\"login"), ["2"]);
    assert_eq!(find(s, "\"login\""), ["2"]);
    assert!(find(s, "\"").contains(&"2".to_string()));
    assert_eq!(find(s, "AND"), ["4", "3"]);
    // "or" is just text: it's inside "words" too.
    assert_eq!(find(s, "OR"), ["3", "1"]);
    assert_eq!(find(s, "NOT"), ["3"]);
    assert_eq!(find(s, "star*"), ["4"]);
    assert_eq!(find(s, "(parens)"), ["4"]);
    assert_eq!(find(s, "col:on"), ["4"]);
    assert_eq!(find(s, "^caret"), ["4"]);
    assert_eq!(find(s, "-minus"), ["4"]);
    assert_eq!(find(s, "100%"), ["5"]);
    assert_eq!(find(s, "%"), ["5"]);
    assert_eq!(find(s, "_"), ["5"]);
    assert!(find(s, "plain*").is_empty());
    assert!(find(s, "NEAR(plain words)").is_empty());
}

pub fn rewrites_reindex<S: HubStore>(s: &mut S) {
    apply(s, &[item("1", "before")], t(0));
    apply(s, &[item("1", "after")], t(1));
    assert!(find(s, "before").is_empty());
    assert_eq!(find(s, "after"), ["1"]);
}

pub fn deletes_unindex<S: HubStore>(s: &mut S) {
    corpus(s);
    apply(s, &[del("item", "b")], t(1));
    assert_eq!(find(s, "login"), ["a"]);
    apply(s, &[del("item", "a"), del("note", "c")], t(2));
    assert!(find(s, "login").is_empty());
    assert!(find(s, "noon").is_empty());
}

pub fn unicode_search<S: HubStore>(s: &mut S) {
    apply(
        s,
        &[
            item("fr", "L'école est fermée"),
            item("jp", "日本語のテキスト"),
            item("emoji", "ship it 🚀 today"),
            item("de", "Straße"),
        ],
        t(0),
    );
    assert_eq!(find(s, "ÉCOLE"), ["fr"]);
    assert_eq!(find(s, "fermée"), ["fr"]);
    assert_eq!(find(s, "日本語"), ["jp"]);
    assert_eq!(find(s, "本"), ["jp"]);
    assert_eq!(find(s, "🚀"), ["emoji"]);
    assert_eq!(find(s, "straße"), ["de"]);
}

pub fn hits_carry_kind_and_current_seq<S: HubStore>(s: &mut S) {
    apply(s, &[put("note", "n", json!({"t": "find me"}))], t(0));
    apply(s, &[item("x", "other")], t(1));
    apply(s, &[put("note", "n", json!({"t": "find me again"}))], t(2));
    assert_eq!(
        s.search("find", 10).unwrap(),
        [SearchHit {
            kind: "note".into(),
            id: "n".into(),
            seq: 3
        }]
    );
}

pub fn repeated_terms_are_fine<S: HubStore>(s: &mut S) {
    corpus(s);
    assert_eq!(find(s, "login login LOGIN"), ["b", "a"]);
}

// ---- leases ----

const TTL: i64 = 30;

fn claim<S: HubStore>(s: &mut S, key: &str, holder: &str, at: i64) -> Claim {
    s.claim_lease(key, holder, Duration::seconds(TTL), t(at))
        .unwrap()
}

pub fn claiming_a_free_key_grants_token_one<S: HubStore>(s: &mut S) {
    let c = claim(s, "item:1", "mac", 0);
    assert!(granted(&c));
    let l = c.lease();
    assert_eq!(
        (l.key.as_str(), l.holder.as_str(), l.token),
        ("item:1", "mac", 1)
    );
    assert_eq!(l.expires_at, t(TTL));
}

pub fn holder_reclaiming_renews_same_token<S: HubStore>(s: &mut S) {
    claim(s, "k", "mac", 0);
    let r = claim(s, "k", "mac", 10);
    assert!(
        matches!(&r, Claim::Renewed { lease } if lease.token == 1 && lease.expires_at == t(10 + TTL))
    );
    // The renewal is stored: it's still held past the first expiry.
    assert!(!claim(s, "k", "phone", TTL + 1).won());
}

pub fn another_holder_is_refused<S: HubStore>(s: &mut S) {
    claim(s, "k", "mac", 0);
    let h = claim(s, "k", "phone", 5);
    assert!(
        matches!(&h, Claim::Held { lease } if lease.holder == "mac" && lease.token == 1 && lease.expires_at == t(TTL))
    );
}

pub fn a_refused_claim_changes_nothing<S: HubStore>(s: &mut S) {
    claim(s, "k", "mac", 0);
    claim(s, "k", "phone", 5);
    claim(s, "k", "phone", 6);
    // mac's lease still ends when it did, and mac still renews it.
    let r = claim(s, "k", "mac", 7);
    assert!(matches!(&r, Claim::Renewed { lease } if lease.token == 1));
    let gone = s.expire_leases(t(7 + TTL)).unwrap();
    assert_eq!(gone.len(), 1);
    assert_eq!(gone[0].holder, "mac");
}

pub fn a_lapsed_lease_is_taken_over_with_the_next_token<S: HubStore>(s: &mut S) {
    claim(s, "k", "mac", 0);
    let g = claim(s, "k", "phone", TTL + 1);
    assert!(matches!(&g, Claim::Granted { lease } if lease.holder == "phone" && lease.token == 2));
    let h = claim(s, "k", "mac", TTL + 2);
    assert!(matches!(&h, Claim::Held { lease } if lease.holder == "phone"));
}

pub fn a_lease_lapses_exactly_at_expiry<S: HubStore>(s: &mut S) {
    claim(s, "k", "mac", 0);
    let just_before = t(TTL) - Duration::nanoseconds(1);
    assert!(
        !s.claim_lease("k", "phone", Duration::seconds(TTL), just_before)
            .unwrap()
            .won()
    );
    assert!(s.expire_leases(just_before).unwrap().is_empty());
    assert!(
        s.claim_lease("k", "phone", Duration::seconds(TTL), t(TTL))
            .unwrap()
            .won()
    );
}

pub fn ttl_is_clamped<S: HubStore>(s: &mut S) {
    let short = s.claim_lease("a", "mac", Duration::zero(), t(0)).unwrap();
    assert_eq!(short.lease().expires_at, t(0) + MIN_TTL);
    let negative = s
        .claim_lease("b", "mac", Duration::seconds(-5), t(0))
        .unwrap();
    assert_eq!(negative.lease().expires_at, t(0) + MIN_TTL);
    let long = s.claim_lease("c", "mac", Duration::days(30), t(0)).unwrap();
    assert_eq!(long.lease().expires_at, t(0) + MAX_TTL);
}

pub fn holder_can_release<S: HubStore>(s: &mut S) {
    claim(s, "k", "mac", 0);
    assert!(s.release_lease("k", "mac", t(1)).unwrap());
    assert!(granted(&claim(s, "k", "phone", 2)));
}

pub fn others_cannot_release<S: HubStore>(s: &mut S) {
    claim(s, "k", "mac", 0);
    assert!(!s.release_lease("k", "phone", t(1)).unwrap());
    assert!(!claim(s, "k", "phone", 2).won());
}

pub fn lapsed_leases_cannot_be_released<S: HubStore>(s: &mut S) {
    claim(s, "k", "mac", 0);
    assert!(!s.release_lease("k", "mac", t(TTL)).unwrap());
}

pub fn releasing_unknown_key_is_false<S: HubStore>(s: &mut S) {
    assert!(!s.release_lease("nope", "mac", t(0)).unwrap());
    claim(s, "k", "mac", 0);
    assert!(s.release_lease("k", "mac", t(1)).unwrap());
    assert!(!s.release_lease("k", "mac", t(1)).unwrap());
}

pub fn tokens_keep_climbing_after_release<S: HubStore>(s: &mut S) {
    for expected in 1..=5 {
        let g = claim(s, "k", if expected % 2 == 0 { "a" } else { "b" }, expected);
        assert_eq!(g.lease().token, expected as u64);
        assert!(
            s.release_lease("k", &g.lease().holder, t(expected))
                .unwrap()
        );
    }
}

pub fn same_holder_after_lapse_gets_a_new_token<S: HubStore>(s: &mut S) {
    claim(s, "k", "mac", 0);
    let g = claim(s, "k", "mac", TTL + 5);
    assert!(matches!(&g, Claim::Granted { lease } if lease.token == 2));
}

pub fn expire_returns_lapsed_sorted_and_removes_them<S: HubStore>(s: &mut S) {
    for key in ["c", "a", "b"] {
        claim(s, key, "mac", 0);
    }
    claim(s, "live", "mac", 20);
    let gone = s.expire_leases(t(TTL + 1)).unwrap();
    let keys: Vec<_> = gone.iter().map(|l| l.key.as_str()).collect();
    assert_eq!(keys, ["a", "b", "c"]);
    assert!(
        gone.iter()
            .all(|l| l.holder == "mac" && l.expires_at == t(TTL))
    );
    // Removed: the old holder can't renew, they get a fresh token.
    assert!(matches!(claim(s, "a", "mac", TTL + 2), Claim::Granted { lease } if lease.token == 2));
}

pub fn expire_leaves_live_leases<S: HubStore>(s: &mut S) {
    claim(s, "k", "mac", 0);
    assert!(s.expire_leases(t(1)).unwrap().is_empty());
    assert!(matches!(claim(s, "k", "mac", 2), Claim::Renewed { .. }));
}

pub fn expire_is_idempotent<S: HubStore>(s: &mut S) {
    assert!(s.expire_leases(t(0)).unwrap().is_empty());
    claim(s, "k", "mac", 0);
    assert_eq!(s.expire_leases(t(100)).unwrap().len(), 1);
    assert!(s.expire_leases(t(100)).unwrap().is_empty());
    assert!(s.expire_leases(t(200)).unwrap().is_empty());
}

pub fn leases_are_per_key<S: HubStore>(s: &mut S) {
    assert!(granted(&claim(s, "item:1", "mac", 0)));
    assert!(granted(&claim(s, "item:2", "phone", 0)));
    assert!(granted(&claim(s, "item:10", "phone", 0)));
    assert!(!claim(s, "item:1", "phone", 1).won());
    assert!(s.release_lease("item:2", "phone", t(1)).unwrap());
    assert!(!claim(s, "item:1", "phone", 1).won());
    assert_eq!(claim(s, "item:2", "mac", 2).lease().token, 2);
    assert_eq!(claim(s, "item:10", "phone", 2).lease().token, 1);
}

pub fn leases_do_not_touch_the_log<S: HubStore>(s: &mut S) {
    apply(s, &[item("1", "x")], t(0));
    claim(s, "k", "mac", 0);
    claim(s, "k", "mac", 1);
    s.release_lease("k", "mac", t(2)).unwrap();
    claim(s, "j", "mac", 3);
    s.expire_leases(t(1000)).unwrap();
    assert_eq!(s.head().unwrap(), 1);
    assert!(find(s, "mac").is_empty());
}

pub fn stale_tokens_lose_after_takeover<S: HubStore>(s: &mut S) {
    use tab_domain::lease::fence_ok;
    let old = claim(s, "k", "mac", 0).lease().clone();
    let new = claim(s, "k", "phone", TTL + 1).lease().clone();
    assert!(new.token > old.token);
    assert!(!fence_ok(Some(&new), old.token, t(TTL + 2)));
    assert!(fence_ok(Some(&new), new.token, t(TTL + 2)));
}

// ---- stats ----

pub fn stats_of_an_empty_store_are_zero<S: HubStore>(s: &mut S) {
    let st = s.stats(t(0)).unwrap();
    assert_eq!((st.records, st.edges, st.live_leases), (0, 0, 0), "{st:?}");
}

pub fn stats_count_records_and_edges<S: HubStore>(s: &mut S) {
    apply(
        s,
        &[
            item("1", "a"),
            item("2", "b"),
            put("note", "1", json!({})),
            link("item:1", "p", "in"),
            link("item:2", "p", "in"),
            link("item:2", "p", "in"),
        ],
        t(0),
    );
    let st = s.stats(t(0)).unwrap();
    assert_eq!((st.records, st.edges), (3, 2));
    apply(s, &[del("item", "1"), item("2", "rewritten")], t(1));
    let st = s.stats(t(1)).unwrap();
    assert_eq!((st.records, st.edges), (2, 1));
}

pub fn stats_count_only_live_leases<S: HubStore>(s: &mut S) {
    claim(s, "a", "mac", 0);
    claim(s, "b", "mac", 10);
    claim(s, "c", "mac", 10);
    s.release_lease("c", "mac", t(11)).unwrap();
    assert_eq!(s.stats(t(11)).unwrap().live_leases, 2);
    // "a" lapsed at TTL even though nothing expired it yet.
    assert_eq!(s.stats(t(TTL)).unwrap().live_leases, 1);
    assert_eq!(s.stats(t(10 + TTL)).unwrap().live_leases, 0);
}

pub fn stats_bytes_grow_with_data<S: HubStore>(s: &mut S) {
    let before = s.stats(t(0)).unwrap().bytes;
    let ops: Vec<_> = (0..300)
        .map(|i| put("item", &i.to_string(), json!({ "title": "x".repeat(500) })))
        .collect();
    apply(s, &ops, t(0));
    let after = s.stats(t(0)).unwrap().bytes;
    assert!(after > before, "{before} -> {after}");
    assert!(
        after >= 300 * 500,
        "{after} bytes can't hold 150 KB of bodies"
    );
}

pub fn stats_do_not_count_the_log_as_records<S: HubStore>(s: &mut S) {
    for i in 0..5 {
        apply(s, &[item("same", &i.to_string())], t(i));
    }
    let st: StoreStats = s.stats(t(5)).unwrap();
    assert_eq!(st.records, 1);
    assert_eq!(s.head().unwrap(), 5);
}
