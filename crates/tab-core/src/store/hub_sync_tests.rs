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
    s.project_set_profile(&p.id, Some("The launch."), None, now())
        .unwrap();
    s.repo_save(&p.id, "acme/launch", None, None, "me", now())
        .unwrap();
    s.resource_save(
        &p.id,
        &crate::store::ResourceInput {
            kind: "website".into(),
            title: "acme.dev".into(),
            url: Some("https://acme.dev".into()),
            ..Default::default()
        },
        "me",
        now(),
    )
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
        "project_repos",
        "project_resources",
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

// ---- Chats ----

fn chat(s: &Store) -> crate::model::Session {
    s.session_create(
        None,
        crate::model::AgentKind::ClaudeCode,
        "Fix login",
        "/w",
        now(),
    )
    .unwrap()
}

/// Everything in the outbox as (table, key, row).
fn outbox(s: &Store) -> Vec<(String, String, Option<Value>)> {
    s.hub_outbox_take(1000)
        .unwrap()
        .into_iter()
        .map(|r| (r.table, r.key, r.row))
        .collect()
}

#[test]
fn chats_sync_with_their_device_and_messages_by_uid() {
    let s = store();
    s.hub_backfill_once().unwrap();
    let c = chat(&s);
    s.message_add(&c.id, "user", "text", "hello", &json!({"x": 1}), now())
        .unwrap();
    let out = outbox(&s);
    let session = out.iter().find(|(t, ..)| t == "sessions").unwrap();
    assert_eq!(session.1, json!([c.id]).to_string());
    let row = session.2.as_ref().unwrap();
    assert_eq!(row["device"], json!(s.device_id().unwrap()));
    assert!(s.device_id().unwrap().starts_with("device-"));
    let msg = out.iter().find(|(t, ..)| t == "session_messages").unwrap();
    let row = msg.2.as_ref().unwrap();
    // Keyed by uid; the local row id stays here.
    assert_eq!(msg.1, json!([row["uid"]]).to_string());
    assert!(row.get("id").is_none(), "{row}");
    assert_eq!(row["content"], "hello");
}

#[test]
fn another_devices_chat_lands_here_in_order() {
    let s = store();
    let here = chat(&s);
    s.message_add(&here.id, "user", "text", "mine", &Value::Null, now())
        .unwrap();
    let queued = s.hub_outbox_len().unwrap();
    let sid = "remote-session";
    let session = json!({
        "id": sid, "item_id": null, "agent": "claude_code", "agent_session_id": null,
        "title": "On the laptop", "status": "working", "cwd": "/Users/x/w",
        "created_at": "2027-01-15T08:00:00.000Z", "updated_at": "2027-01-15T08:00:00.000Z",
        "key": null, "brief": null, "device": "device-laptop",
    });
    let msg = |uid: &str, text: &str| {
        (
            "session_messages".to_string(),
            json!([uid]).to_string(),
            Some(json!({
                "uid": uid, "session_id": sid, "role": "assistant", "kind": "text",
                "content": text, "meta": "null", "created_at": "2027-01-15T08:00:01.000Z",
            })),
        )
    };
    let changed = s
        .hub_apply_remote(&[
            ("sessions".into(), json!([sid]).to_string(), Some(session)),
            msg("u1", "first"),
            msg("u2", "second"),
        ])
        .unwrap();
    assert_eq!(changed.len(), 3);
    let got = s.session_get(sid).unwrap().unwrap();
    assert_eq!(got.device.as_deref(), Some("device-laptop"));
    let texts: Vec<String> = s
        .messages(sid)
        .unwrap()
        .into_iter()
        .map(|m| m.content)
        .collect();
    assert_eq!(texts, ["first", "second"]);
    // Their ids are this copy's own, after the local message's.
    let local_id = s.messages(&here.id).unwrap()[0].id;
    assert!(s.messages(sid).unwrap().iter().all(|m| m.id > local_id));
    assert_eq!(s.message_by_uid("u2").unwrap().unwrap().content, "second");
    // Writing them didn't queue anything to send back.
    assert_eq!(s.hub_outbox_len().unwrap(), queued);
    // The same message again changes nothing.
    assert!(
        s.hub_apply_remote(&[msg("u1", "first")])
            .unwrap()
            .is_empty()
    );
}

#[test]
fn huge_tool_output_is_cut_off_in_the_hub_but_kept_here() {
    let s = store();
    s.hub_backfill_once().unwrap();
    let c = chat(&s);
    let big = "x".repeat(MAX_SYNCED_CONTENT + 10);
    s.message_add(&c.id, "tool", "tool", &big, &Value::Null, now())
        .unwrap();
    let out = outbox(&s);
    let (_, _, row) = out.iter().find(|(t, ..)| t == "session_messages").unwrap();
    let synced = row.as_ref().unwrap()["content"].as_str().unwrap();
    assert!(synced.len() < big.len() + 100 && synced.len() > MAX_SYNCED_CONTENT);
    assert!(synced.contains("cut off"));
    assert_eq!(s.messages(&c.id).unwrap()[0].content, big);
    assert!(serde_json::to_vec(row).unwrap().len() <= tab_domain::ops::MAX_BODY_BYTES);
}

#[test]
fn a_copy_synced_before_chats_did_sends_its_chats_once() {
    let s = Store::in_memory().unwrap();
    let c = chat(&s);
    s.message_add(&c.id, "user", "text", "old", &Value::Null, now())
        .unwrap();
    // As an older version left it: introduced to its hub, no table list.
    s.hub_sync_install().unwrap();
    {
        let conn = s.conn();
        conn.execute_batch(
            "DELETE FROM hub_outbox; DELETE FROM hub_tables; UPDATE hub_sync SET backfilled = 1;",
        )
        .unwrap();
    }
    s.hub_sync_install().unwrap();
    let mut tables: Vec<String> = outbox(&s).into_iter().map(|(t, ..)| t).collect();
    tables.sort();
    assert_eq!(tables, ["session_messages", "sessions"]);
    // Only once.
    s.hub_outbox_done(i64::MAX).unwrap();
    s.hub_sync_install().unwrap();
    assert_eq!(s.hub_outbox_len().unwrap(), 0);
}

#[test]
fn only_what_the_person_reads_in_a_chat_is_synced() {
    let s = store();
    s.hub_backfill_once().unwrap();
    let c = chat(&s);
    let input = json!({ "file_path": "src/a.rs", "content": "x".repeat(50_000) });
    s.message_add(
        &c.id,
        "assistant",
        "text",
        "Fixing it.",
        &Value::Null,
        now(),
    )
    .unwrap();
    s.message_add(
        &c.id,
        "assistant",
        "tool",
        "Write src/a.rs",
        &json!({ "tool": "Write", "id": "t1", "input": input }),
        now(),
    )
    .unwrap();
    s.message_add(
        &c.id,
        "tool",
        "tool_result",
        "File written.\n...",
        &json!({"tool_use_id": "t1"}),
        now(),
    )
    .unwrap();
    let rows: Vec<Value> = outbox(&s)
        .into_iter()
        .filter(|(t, ..)| t == "session_messages")
        .filter_map(|(.., r)| r)
        .collect();
    let kinds: Vec<&str> = rows.iter().map(|r| r["kind"].as_str().unwrap()).collect();
    // Tool output stays here.
    assert_eq!(kinds, ["text", "tool"]);
    // The call goes as its summary and tool name, without its input.
    let tool = &rows[1];
    assert_eq!(tool["content"], "Write src/a.rs");
    assert_eq!(
        serde_json::from_str::<Value>(tool["meta"].as_str().unwrap()).unwrap(),
        json!({ "tool": "Write" })
    );
    assert!(serde_json::to_string(tool).unwrap().len() < 1000);
    // Everything is still here in full.
    let here = s.messages(&c.id).unwrap();
    assert_eq!(here.len(), 3);
    assert_eq!(here[1].meta["input"]["file_path"], "src/a.rs");
    // A fresh hub isn't sent tool output either.
    assert!(
        s.hub_local_keys()
            .unwrap()
            .iter()
            .filter(|(t, _)| t == "session_messages")
            .count()
            == 2
    );
}

#[test]
fn step_lists_and_questions_sync_with_their_input() {
    let s = store();
    s.hub_backfill_once().unwrap();
    let c = chat(&s);
    let todos = json!({ "tool": "TodoWrite", "input": { "todos": [{ "content": "Fix it", "status": "pending" }] } });
    let ask = json!({ "tool": "AskUserQuestion", "input": { "questions": [{ "question": "Which db?" }] } });
    let steps = json!({ "tool": "mcp__tab__tab_steps", "input": { "steps": [] } });
    for meta in [&todos, &ask, &steps] {
        s.message_add(&c.id, "assistant", "tool", "…", meta, now())
            .unwrap();
    }
    let metas: Vec<Value> = outbox(&s)
        .into_iter()
        .filter(|(t, ..)| t == "session_messages")
        .filter_map(|(.., r)| r)
        .map(|r| serde_json::from_str(r["meta"].as_str().unwrap()).unwrap())
        .collect();
    assert_eq!(metas, [todos, ask, steps]);
}

#[test]
fn hub_page_counts_only_synced_tables_and_they_arrive_on_the_other_copy() {
    for (_, table) in SYNCED_SUMMARY {
        assert!(
            SYNCED.iter().any(|(t, _)| t == table),
            "{table} is listed as synced on the Hub page but isn't in SYNCED"
        );
    }
    let (a, b) = (store(), store());
    populate(&a);
    sync(&a, &b);
    let count = |s: &Store, what: &str| {
        s.hub_synced_counts()
            .unwrap()
            .into_iter()
            .find(|c| c.what == what)
            .unwrap()
            .count
    };
    for what in ["Queue items", "Projects", "Project notes", "Workflows"] {
        assert!(count(&a, what) > 0, "{what}");
        assert_eq!(count(&a, what), count(&b, what), "{what}");
    }
}
