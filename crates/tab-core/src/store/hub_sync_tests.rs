use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use super::*;
use crate::model::{ContextSection, EntityRef, IncomingEvent, ItemState, NewContext};

fn now() -> DateTime<Utc> {
    DateTime::from_timestamp(1_800_000_000, 0).unwrap()
}

fn store() -> Store {
    let s = Store::in_memory().unwrap();
    s.hub_sync_install().unwrap();
    s
}

fn event(id: &str, title: &str) -> IncomingEvent {
    IncomingEvent {
        source: "github".into(),
        external_id: id.into(),
        kind: "github.review_requested".into(),
        title: title.into(),
        occurred_at: now(),
        entities: vec![
            EntityRef {
                kind: "repo".into(),
                key: "github:acme/api".into(),
                label: "acme/api".into(),
                rel: "in".into(),
            },
            EntityRef {
                kind: "person".into(),
                key: "github:dana".into(),
                label: "dana".into(),
                rel: "from".into(),
            },
        ],
        ..Default::default()
    }
}

/// Every synced table, row by row, sorted: what two copies must agree on.
fn dump(s: &Store) -> Vec<(String, String)> {
    let conn = s.conn.lock().unwrap();
    let mut out = vec![];
    for (table, _) in SYNCED {
        let cols: Vec<String> = conn
            .prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        let object: Vec<String> = cols.iter().map(|c| format!("'{c}', {c}")).collect();
        let mut rows: Vec<String> = conn
            .prepare(&format!(
                "SELECT json_object({}) FROM {table}",
                object.join(", ")
            ))
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        rows.sort();
        out.extend(rows.into_iter().map(|r| (table.to_string(), r)));
    }
    out
}

/// Sends everything in `from`'s outbox to `to`, the way the hub would.
fn sync(from: &Store, to: &Store) -> usize {
    let rows = from.hub_outbox_take(10_000).unwrap();
    let n = rows.len();
    if let Some(last) = rows.last() {
        from.hub_outbox_done(last.seq).unwrap();
    }
    let changes: Vec<_> = rows.into_iter().map(|r| (r.table, r.key, r.row)).collect();
    to.hub_apply_remote(&changes).unwrap();
    n
}

fn inserted(u: crate::store::Upsert) -> crate::model::QueueItem {
    match u {
        crate::store::Upsert::Inserted(i) | crate::store::Upsert::Updated(i) => i,
        crate::store::Upsert::Unchanged => panic!("expected a write"),
    }
}

fn populate(s: &Store) -> String {
    let item = inserted(s.upsert(&event("1", "Review login"), now()).unwrap());
    s.upsert(&event("2", "Fix tests"), now()).unwrap();
    let p = s
        .project_save(None, "Launch", "blue", "rocket", now())
        .unwrap();
    s.item_set_project(&item.id, Some(&p.id), now()).unwrap();
    s.note_save(&p.id, "Plan", "Ship Friday", "me", None, &[], now())
        .unwrap();
    s.context_add(
        &item.id,
        &NewContext {
            section: ContextSection::Context,
            ref_item_id: None,
            url: Some("https://example.com/spec".into()),
            title: "Spec".into(),
            summary: Some("what we're building".into()),
            added_by: "me".into(),
        },
        now(),
    )
    .unwrap();
    let w = crate::workflow::templates().remove(0);
    s.workflow_save(&w, "me", now()).unwrap();
    s.set_state(&item.id, ItemState::Done, None, now()).unwrap();
    item.id
}

#[test]
fn every_synced_table_round_trips_through_the_outbox() {
    let (a, b) = (store(), store());
    populate(&a);
    assert!(sync(&a, &b) > 0);
    let (da, db) = (dump(&a), dump(&b));
    let tables: std::collections::BTreeSet<_> = da.iter().map(|(t, _)| t.as_str()).collect();
    for t in [
        "items",
        "projects",
        "project_notes",
        "item_context",
        "entities",
        "item_entities",
        "workflows",
        "workflow_versions",
    ] {
        assert!(
            tables.contains(t),
            "populate() should write {t}: {tables:?}"
        );
    }
    assert_eq!(da, db);
    // Applying didn't echo into b's outbox.
    assert_eq!(b.hub_outbox_len().unwrap(), 0);
}

#[test]
fn changes_and_deletes_follow() {
    let (a, b) = (store(), store());
    let id = populate(&a);
    sync(&a, &b);
    a.item_set_title(&id, "Renamed", &json!({"named_by": "me"}), now())
        .unwrap();
    let p = a.projects().unwrap().remove(0);
    a.project_delete(&p.id).unwrap();
    sync(&a, &b);
    assert_eq!(dump(&a), dump(&b));
    assert!(b.projects().unwrap().is_empty());
}

#[test]
fn changes_flow_both_ways_without_echo() {
    let (a, b) = (store(), store());
    let id = populate(&a);
    sync(&a, &b);
    b.set_state(&id, ItemState::Open, None, now()).unwrap();
    assert_eq!(sync(&b, &a), 1);
    assert_eq!(a.get(&id).unwrap().unwrap().state, ItemState::Open);
    assert_eq!(a.hub_outbox_len().unwrap(), 0);
    assert_eq!(dump(&a), dump(&b));
}

#[test]
fn the_outbox_sees_the_latest_row_and_gone_rows() {
    let s = store();
    let item = inserted(s.upsert(&event("1", "First"), now()).unwrap());
    s.set_state(&item.id, ItemState::Done, None, now()).unwrap();
    let rows = s.hub_outbox_take(1000).unwrap();
    let mine: Vec<_> = rows.iter().filter(|r| r.table == "items").collect();
    assert!(mine.len() >= 2);
    assert!(
        mine.iter()
            .all(|r| r.row.as_ref().unwrap()["state"] == "done")
    );
    assert_eq!(mine[0].key, json!([item.id]).to_string());

    let p = s.project_save(None, "Gone", "red", "x", now()).unwrap();
    s.project_delete(&p.id).unwrap();
    let rows = s.hub_outbox_take(1000).unwrap();
    let gone: Vec<_> = rows.iter().filter(|r| r.table == "projects").collect();
    assert!(gone.iter().all(|r| r.row.is_none()));
    // Acknowledging clears exactly what was taken.
    s.hub_outbox_done(rows.last().unwrap().seq).unwrap();
    assert_eq!(s.hub_outbox_len().unwrap(), 0);
}

#[test]
fn backfill_queues_existing_rows_once() {
    let s = Store::in_memory().unwrap();
    s.upsert(&event("1", "Before sync existed"), now()).unwrap();
    s.hub_sync_install().unwrap();
    assert_eq!(s.hub_outbox_len().unwrap(), 0);
    let n = s.hub_backfill_once().unwrap();
    assert!(n >= 1 + 2 + 2, "item, 2 entities, 2 links: {n}");
    assert_eq!(s.hub_backfill_once().unwrap(), 0);
    // Installing again (next launch) changes nothing.
    s.hub_sync_install().unwrap();
    assert_eq!(s.hub_backfill_once().unwrap(), 0);
    let b = store();
    sync(&s, &b);
    assert_eq!(dump(&s), dump(&b));
}

#[test]
fn remote_rows_tolerate_unknown_tables_and_extra_columns() {
    let s = store();
    let mut row = serde_json::to_value(
        json!({"id": "p1", "name": "From phone", "color": "red", "icon": "x",
        "created_at": "2026-11-07T00:00:00.000Z", "updated_at": "2026-11-07T00:00:00.000Z",
        "added_in_a_newer_version": 42}),
    )
    .unwrap();
    s.hub_apply_remote(&[
        (
            "projects".into(),
            json!(["p1"]).to_string(),
            Some(row.clone()),
        ),
        (
            "future_table".into(),
            json!(["x"]).to_string(),
            Some(json!({"id": "x"})),
        ),
    ])
    .unwrap();
    assert_eq!(s.projects().unwrap()[0].name, "From phone");
    row["name"] = Value::String("Renamed on phone".into());
    s.hub_apply_remote(&[("projects".into(), json!(["p1"]).to_string(), Some(row))])
        .unwrap();
    assert_eq!(s.projects().unwrap()[0].name, "Renamed on phone");
    s.hub_apply_remote(&[("projects".into(), json!(["p1"]).to_string(), None)])
        .unwrap();
    assert!(s.projects().unwrap().is_empty());
    assert_eq!(s.hub_outbox_len().unwrap(), 0);
    // A row that can't be written is skipped, not a reason to stop syncing.
    let changed = s
        .hub_apply_remote(&[
            (
                "projects".into(),
                json!(["p2"]).to_string(),
                Some(json!("nope")),
            ),
            (
                "projects".into(),
                json!(["p3"]).to_string(),
                Some(json!({"id": "p3"})),
            ),
        ])
        .unwrap();
    assert!(changed.is_empty(), "{changed:?}");
}

#[test]
fn children_arriving_before_their_parents_still_land() {
    let (a, b) = (store(), store());
    populate(&a);
    let mut rows = a.hub_outbox_take(10_000).unwrap();
    // Dedupe to the latest of each row, then send children first.
    let mut seen = std::collections::HashSet::new();
    rows.retain(|r| seen.insert((r.table.clone(), r.key.clone())));
    rows.reverse();
    let changes: Vec<_> = rows.into_iter().map(|r| (r.table, r.key, r.row)).collect();
    b.hub_apply_remote(&changes).unwrap();
    assert_eq!(dump(&a), dump(&b));
}
