use chrono::{DateTime, Utc};
use serde_json::json;
use tab_domain::ops::Op;
use tab_protocol::pb::{self, client_envelope::Body as Req, server_envelope::Body as Res};
use tab_protocol::{ClientEnvelope, encode};
use tab_store::mem::MemStore;
use tab_store::sqlite::SqliteStore;

use super::*;

fn t(s: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(1_800_000_000 + s, 0).unwrap()
}

fn put(id: &str, title: &str) -> pb::Op {
    (&Op::Put {
        kind: "item".into(),
        id: id.into(),
        body: json!({ "title": title }),
    })
        .into()
}

/// Sends a request as a binary frame, the way a socket would.
const MAC: ConnId = 1;
const PHONE: ConnId = 2;

fn send<S: HubStore>(
    hub: &mut Hub<S>,
    conn: ConnId,
    id: u64,
    req: Req,
    now: DateTime<Utc>,
) -> Handled {
    hub.handle_frame(conn, &encode(&ClientEnvelope::new(id, req)), now)
}

fn hello(device: &str, since: u64) -> Req {
    Req::Hello(pb::Hello {
        device: device.into(),
        client: "test/1".into(),
        since,
    })
}

fn body(h: &Handled) -> &Res {
    h.reply.body.as_ref().unwrap()
}

fn session<S: HubStore>(mut hub: Hub<S>) {
    hub.connect(MAC, "in-process", t(0));
    hub.connect(PHONE, "192.168.1.20", t(0));
    // Nothing but hello until a connection says who it is.
    let h = send(
        &mut hub,
        MAC,
        1,
        Req::GetChanges(pb::GetChanges::default()),
        t(0),
    );
    assert!(matches!(body(&h), Res::Error(e) if e.message.contains("hello")));
    let h = send(&mut hub, MAC, 1, Req::Hello(pb::Hello::default()), t(0));
    assert!(matches!(body(&h), Res::Error(e) if e.message.contains("device")));
    assert_eq!(hub.subscribers().count(), 0);
    let h = send(&mut hub, MAC, 1, hello("mac", 0), t(0));
    assert!(matches!(body(&h), Res::Welcome(w) if w.head == 0 && w.protocol_version == 1));

    // A write replies with its sequence numbers and pushes the changes.
    let h = send(
        &mut hub,
        MAC,
        2,
        Req::Apply(pb::Apply {
            ops: vec![put("1", "Fix login"), put("2", "Review docs")],
        }),
        t(1),
    );
    assert_eq!(h.reply.request_id, 2);
    assert!(matches!(body(&h), Res::Applied(a) if (a.first, a.last) == (1, 2)));
    let push = h.push.unwrap();
    assert_eq!(push.request_id, 0);
    assert!(
        matches!(&push.body, Some(Res::Changes(c)) if c.changes.len() == 2 && c.head == 2 && c.changes[0].by == "mac")
    );

    send(&mut hub, PHONE, 30, hello("phone", 0), t(2));
    assert_eq!(hub.subscribers().collect::<Vec<_>>(), [MAC, PHONE]);
    // Catching up pages.
    let h = send(
        &mut hub,
        PHONE,
        3,
        Req::GetChanges(pb::GetChanges { since: 0, limit: 1 }),
        t(2),
    );
    assert!(matches!(body(&h), Res::Changes(c) if c.changes.len() == 1 && c.more));
    let h = send(
        &mut hub,
        PHONE,
        4,
        Req::GetChanges(pb::GetChanges { since: 1, limit: 0 }),
        t(2),
    );
    assert!(matches!(body(&h), Res::Changes(c) if c.changes.len() == 1 && !c.more));

    let h = send(
        &mut hub,
        PHONE,
        5,
        Req::Search(pb::Search {
            query: "login".into(),
            limit: 0,
        }),
        t(2),
    );
    assert!(matches!(body(&h), Res::SearchResults(r) if r.hits.len() == 1 && r.hits[0].id == "1"));

    let h = send(
        &mut hub,
        PHONE,
        6,
        Req::GetRecord(pb::GetRecord {
            kind: "item".into(),
            id: "2".into(),
        }),
        t(2),
    );
    let Res::Record(pb::RecordReply { record: Some(r) }) = body(&h) else {
        panic!("{h:?}")
    };
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&r.body_json).unwrap(),
        json!({"title": "Review docs"})
    );

    // Leases belong to the device that asked.
    let claim = |hub: &mut Hub<S>, conn: ConnId, now| {
        let h = send(
            hub,
            conn,
            7,
            Req::ClaimLease(pb::ClaimLease {
                key: "item:1".into(),
                ttl_secs: 30,
            }),
            now,
        );
        match body(&h) {
            Res::LeaseClaim(c) => c.clone(),
            other => panic!("{other:?}"),
        }
    };
    assert_eq!(
        claim(&mut hub, MAC, t(3)).outcome,
        pb::lease_claim::Outcome::Granted as i32
    );
    let held = claim(&mut hub, PHONE, t(4));
    assert_eq!(held.outcome, pb::lease_claim::Outcome::Held as i32);
    assert_eq!(held.lease.unwrap().holder, "mac");
    assert_eq!(hub.tick(t(10)).unwrap(), None);
    let push = hub.tick(t(40)).unwrap().unwrap();
    assert!(matches!(&push.body, Some(Res::LeasesExpired(e)) if e.leases.len() == 1));

    // Bad input gets an error with the request id, and writes nothing.
    let bad = pb::Op {
        op: Some(pb::op::Op::Put(pb::Put {
            kind: "item".into(),
            id: "3".into(),
            body_json: b"nope".to_vec(),
        })),
    };
    let h = send(
        &mut hub,
        MAC,
        8,
        Req::Apply(pb::Apply {
            ops: vec![put("3", "ok"), bad],
        }),
        t(5),
    );
    assert_eq!(h.reply.request_id, 8);
    assert!(matches!(body(&h), Res::Error(e) if e.code == pb::error::Code::BadRequest as i32));
    assert!(h.push.is_none());
    let h = send(
        &mut hub,
        MAC,
        9,
        Req::Apply(pb::Apply {
            ops: vec![put("", "no id")],
        }),
        t(5),
    );
    assert!(matches!(body(&h), Res::Error(e) if e.code == pb::error::Code::BadRequest as i32));
    assert_eq!(hub.store().head().unwrap(), 2);
    let h = hub.handle_frame(MAC, b"\xff\xff", t(5));
    assert!(matches!(body(&h), Res::Error(_)));
    let h = send(&mut hub, 99, 10, hello("ghost", 0), t(5));
    assert!(matches!(body(&h), Res::Error(e) if e.message.contains("unknown connection")));

    // Status: what's stored and who's connected, and how far along.
    let h = send(
        &mut hub,
        PHONE,
        11,
        Req::GetHubStatus(pb::GetHubStatus {}),
        t(6),
    );
    let Res::HubStatus(st) = body(&h) else {
        panic!("{h:?}")
    };
    assert_eq!(st.runs_on, "local");
    assert_eq!(
        (st.head, st.records, st.edges, st.live_leases),
        (2, 2, 0, 0)
    );
    assert!(st.storage_bytes > 0);
    let devices: Vec<_> = st
        .connections
        .iter()
        .map(|c| (c.device.as_str(), c.address.as_str(), c.synced_to))
        .collect();
    assert_eq!(
        devices,
        [("mac", "in-process", 2), ("phone", "192.168.1.20", 2)]
    );
    hub.disconnect(PHONE);
    assert_eq!(hub.status(t(7)).unwrap().connections.len(), 1);
}

#[test]
fn a_session_in_memory() {
    session(Hub::new(MemStore::new(), "local", t(0)));
}

#[test]
fn a_session_on_sqlite() {
    session(Hub::new(SqliteStore::in_memory().unwrap(), "local", t(0)));
}

fn history<S: HubStore>(s: &mut S) {
    s.apply("mac", &[put("1", "one").try_into().unwrap()], t(0))
        .unwrap();
    s.apply(
        "phone",
        &[
            put("2", "two").try_into().unwrap(),
            Op::Link {
                from: "item:1".into(),
                to: "item:2".into(),
                rel: "blocks".into(),
            },
        ],
        t(1),
    )
    .unwrap();
    s.apply(
        "mac",
        &[Op::Delete {
            kind: "item".into(),
            id: "1".into(),
        }],
        t(2),
    )
    .unwrap();
    s.apply("mac", &[put("3", "three").try_into().unwrap()], t(2))
        .unwrap();
}

#[test]
fn migrating_local_to_cloud_and_back_keeps_everything() {
    // "Local" is SQLite; the in-memory store stands in for the cloud's.
    let mut local = SqliteStore::in_memory().unwrap();
    history(&mut local);
    let mut cloud = MemStore::new();
    let mut seen = vec![];
    let head = migrate::copy(&local, &mut cloud, |s| seen.push(s)).unwrap();
    assert_eq!(head, 5);
    assert_eq!(seen, [5]);
    assert_eq!(
        cloud.changes_since(0, 100).unwrap(),
        local.changes_since(0, 100).unwrap()
    );
    assert_eq!(
        cloud.search("three", 5).unwrap(),
        local.search("three", 5).unwrap()
    );
    assert_eq!(
        cloud.graph_neighbors("item:2", 5).unwrap(),
        local.graph_neighbors("item:2", 5).unwrap()
    );

    let mut back = SqliteStore::in_memory().unwrap();
    migrate::copy(&cloud, &mut back, |_| {}).unwrap();
    assert_eq!(
        back.changes_since(0, 100).unwrap(),
        local.changes_since(0, 100).unwrap()
    );
    assert_eq!(
        back.get("item", "2").unwrap(),
        local.get("item", "2").unwrap()
    );

    // Never into a hub that's in use.
    assert!(matches!(
        migrate::copy(&cloud, &mut back, |_| {}),
        Err(migrate::MigrateError::NotEmpty { head: 5 })
    ));
}

#[test]
fn sync_positions_follow_what_each_connection_was_sent() {
    let mut hub = Hub::new(MemStore::new(), "cloud", t(0));
    for (conn, name) in [(1, "mac"), (2, "phone"), (3, "runner")] {
        hub.connect(conn, "", t(0));
        send(&mut hub, conn, 1, hello(name, 0), t(0));
    }
    let synced = |hub: &Hub<MemStore>| -> Vec<u64> {
        hub.status(t(9))
            .unwrap()
            .connections
            .iter()
            .map(|c| c.synced_to)
            .collect()
    };
    // A write is pushed to everyone, so everyone is at the new head.
    send(
        &mut hub,
        1,
        2,
        Req::Apply(pb::Apply {
            ops: vec![put("1", "a"), put("2", "b")],
        }),
        t(1),
    );
    assert_eq!(synced(&hub), [2, 2, 2]);
    // A device that reconnects says where it was, then catches up by paging.
    hub.disconnect(3);
    send(
        &mut hub,
        1,
        3,
        Req::Apply(pb::Apply {
            ops: vec![put("3", "c")],
        }),
        t(2),
    );
    hub.connect(4, "", t(3));
    send(&mut hub, 4, 1, hello("runner", 2), t(3));
    assert_eq!(synced(&hub), [3, 3, 2]);
    send(
        &mut hub,
        4,
        2,
        Req::GetChanges(pb::GetChanges { since: 2, limit: 0 }),
        t(3),
    );
    assert_eq!(synced(&hub), [3, 3, 3]);
    // A writer that was behind stays behind until it catches up.
    send(&mut hub, 4, 3, hello("runner", 0), t(4));
    send(
        &mut hub,
        4,
        4,
        Req::Apply(pb::Apply {
            ops: vec![put("4", "d")],
        }),
        t(4),
    );
    assert_eq!(synced(&hub), [4, 4, 0]);
    let st = hub.status(t(9)).unwrap();
    assert_eq!(st.runs_on, "cloud");
    assert_eq!(st.started_at.unwrap().seconds, t(0).timestamp());
}

// ---- Live channel ----

const LAPTOP: ConnId = 3;

fn session_row(id: &str, device: &str) -> pb::Op {
    let (kind, rid) = live::session_record(id);
    (&Op::Put {
        kind: kind.into(),
        id: rid,
        body: json!({ "id": id, "device": device, "title": "Fix login" }),
    })
        .into()
}

fn watch(sessions: &[&str]) -> Req {
    Req::Watch(pb::Watch {
        sessions: sessions.iter().map(|s| s.to_string()).collect(),
    })
}

fn publish(session: &str, event: serde_json::Value) -> Req {
    Req::Publish(pb::Publish {
        session: session.into(),
        event_json: event.to_string().into_bytes(),
    })
}

fn steer(session: &str, cmd: serde_json::Value) -> Req {
    Req::Steer(pb::Steer {
        session: session.into(),
        body_json: cmd.to_string().into_bytes(),
    })
}

fn delivered(h: &Handled) -> u32 {
    match body(h) {
        Res::Delivered(d) => d.connections,
        other => panic!("expected Delivered, got {other:?}"),
    }
}

fn error(h: &Handled) -> (i32, String) {
    match body(h) {
        Res::Error(e) => (e.code, e.message.clone()),
        other => panic!("expected an error, got {other:?}"),
    }
}

/// The mac runs session "s1"; the phone and laptop watch and steer it.
fn live_hub<S: HubStore>(store: S) -> Hub<S> {
    let mut hub = Hub::new(store, "local", t(0));
    for (conn, device) in [(MAC, "mac"), (PHONE, "phone"), (LAPTOP, "laptop")] {
        hub.connect(conn, "test", t(0));
        send(&mut hub, conn, 1, hello(device, 0), t(0));
    }
    let h = send(
        &mut hub,
        MAC,
        2,
        Req::Apply(pb::Apply {
            ops: vec![session_row("s1", "mac")],
        }),
        t(1),
    );
    assert!(matches!(body(&h), Res::Applied(_)));
    hub
}

fn live_events<S: HubStore>(store: S) {
    let mut hub = live_hub(store);
    assert_eq!(hub.runner("s1").as_deref(), Some("mac"));
    assert_eq!(hub.runner("nope"), None);

    // Nobody watches yet: publishing reaches no one, and is fine.
    let h = send(
        &mut hub,
        MAC,
        3,
        publish("s1", json!({"delta": "Hel"})),
        t(2),
    );
    assert_eq!(delivered(&h), 0);
    assert!(h.direct.is_empty() && h.push.is_none());

    // The phone starts watching; the runner hears about it so it can send
    // a snapshot.
    let h = send(&mut hub, PHONE, 4, watch(&["s1"]), t(2));
    assert_eq!(h.direct.len(), 1);
    let (to, notice) = &h.direct[0];
    assert_eq!(*to, MAC);
    assert!(matches!(&notice.body, Some(Res::Watched(w)) if w.session == "s1" && w.watchers == 1));

    // Events go to watchers only, untouched, and aren't stored.
    let head = hub.store().head().unwrap();
    let h = send(
        &mut hub,
        MAC,
        5,
        publish("s1", json!({"delta": "lo"})),
        t(3),
    );
    assert_eq!(delivered(&h), 1);
    assert_eq!(h.direct.len(), 1);
    let (to, live) = &h.direct[0];
    assert_eq!(*to, PHONE);
    assert_eq!(live.request_id, 0);
    let Some(Res::Live(l)) = &live.body else {
        panic!("{live:?}")
    };
    assert_eq!((l.session.as_str(), l.from.as_str()), ("s1", "mac"));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&l.event_json).unwrap(),
        json!({"delta": "lo"})
    );
    assert_eq!(hub.store().head().unwrap(), head);

    // Only the runner may publish for a session.
    let h = send(
        &mut hub,
        LAPTOP,
        6,
        publish("s1", json!({"delta": "x"})),
        t(3),
    );
    assert_eq!(error(&h).0, pb::error::Code::Conflict as i32);
    assert!(h.direct.is_empty());
    let h = send(&mut hub, MAC, 7, publish("ghost", json!({})), t(3));
    assert_eq!(error(&h).0, pb::error::Code::BadRequest as i32);

    // A second watcher; both get events.
    send(&mut hub, LAPTOP, 8, watch(&["s1", "other"]), t(4));
    let h = send(&mut hub, MAC, 9, publish("s1", json!({"delta": "!"})), t(4));
    let mut to: Vec<ConnId> = h.direct.iter().map(|(c, _)| *c).collect();
    to.sort();
    assert_eq!(to, [PHONE, LAPTOP]);

    // Watching replaces the set; the runner hears the new count.
    let h = send(&mut hub, PHONE, 10, watch(&[]), t(5));
    assert!(
        matches!(&h.direct[..], [(MAC, n)] if matches!(&n.body, Some(Res::Watched(w)) if w.watchers == 1))
    );
    let h = send(
        &mut hub,
        MAC,
        11,
        publish("s1", json!({"delta": "?"})),
        t(5),
    );
    assert_eq!(
        h.direct.iter().map(|(c, _)| *c).collect::<Vec<_>>(),
        [LAPTOP]
    );

    // A watcher leaving tells the runner too.
    let out = hub.disconnect(LAPTOP);
    assert!(
        matches!(&out[..], [(MAC, n)] if matches!(&n.body, Some(Res::Watched(w)) if w.watchers == 0))
    );
    assert!(hub.watchers("s1").is_empty());
}

fn live_steering<S: HubStore>(store: S) {
    let mut hub = live_hub(store);

    // Steering from the phone reaches the mac, which runs the session.
    let cmd = json!({"type": "message", "text": "use the staging db"});
    let h = send(&mut hub, PHONE, 3, steer("s1", cmd.clone()), t(2));
    assert_eq!(delivered(&h), 1);
    let [(to, env)] = &h.direct[..] else {
        panic!("{h:?}")
    };
    assert_eq!(*to, MAC);
    let Some(Res::Steered(s)) = &env.body else {
        panic!("{env:?}")
    };
    assert_eq!((s.session.as_str(), s.from.as_str()), ("s1", "phone"));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&s.body_json).unwrap(),
        cmd
    );
    // Nothing went into the log.
    assert_eq!(hub.store().head().unwrap(), 1);

    // An unknown session, or a runner that isn't connected, is refused.
    let h = send(&mut hub, PHONE, 4, steer("ghost", json!({})), t(2));
    assert_eq!(error(&h).0, pb::error::Code::BadRequest as i32);
    hub.disconnect(MAC);
    let h = send(&mut hub, PHONE, 5, steer("s1", json!({})), t(2));
    let (code, message) = error(&h);
    assert_eq!(code, pb::error::Code::Conflict as i32);
    assert!(message.contains("mac"), "{message}");

    // The record says where it runs: moving it moves the steering.
    let h = send(
        &mut hub,
        PHONE,
        6,
        Req::Apply(pb::Apply {
            ops: vec![session_row("s1", "laptop")],
        }),
        t(3),
    );
    assert!(matches!(body(&h), Res::Applied(_)));
    let h = send(
        &mut hub,
        PHONE,
        7,
        steer("s1", json!({"type": "stop"})),
        t(3),
    );
    assert_eq!(
        h.direct.iter().map(|(c, _)| *c).collect::<Vec<_>>(),
        [LAPTOP]
    );
}

#[test]
fn live_events_in_memory() {
    live_events(MemStore::new());
}

#[test]
fn live_events_on_sqlite() {
    live_events(SqliteStore::in_memory().unwrap());
}

#[test]
fn live_steering_in_memory() {
    live_steering(MemStore::new());
}

#[test]
fn live_steering_on_sqlite() {
    live_steering(SqliteStore::in_memory().unwrap());
}

#[test]
fn live_requests_need_hello_first() {
    let mut hub = Hub::new(MemStore::new(), "local", t(0));
    hub.connect(MAC, "test", t(0));
    for req in [
        watch(&["s1"]),
        publish("s1", json!({})),
        steer("s1", json!({})),
    ] {
        let h = send(&mut hub, MAC, 1, req, t(0));
        assert!(error(&h).1.contains("hello"));
    }
}

// ---- Files ----

fn file_cleanup<S: HubStore>(mut store: S) {
    use tab_store::blob::{BlobStore, MemBlobStore};
    let mut blobs = MemBlobStore::new();
    let kept = blobs.put(b"in a message", t(0)).unwrap();
    let orphan = blobs.put(b"nobody refers to me", t(0)).unwrap();
    let fresh = blobs.put(b"record on its way", t(100_000)).unwrap();
    let dropped = blobs.put(b"message deleted", t(0)).unwrap();
    store
        .apply(
            "mac",
            &[
                Op::Put {
                    kind: "session_messages".into(),
                    id: "[\"m1\"]".into(),
                    // As the desktop syncs it: meta is JSON text.
                    body: json!({ "meta": json!({ "files": [{ "hash": kept.hash }] }).to_string() }),
                },
                Op::Put {
                    kind: "session_messages".into(),
                    id: "[\"m2\"]".into(),
                    body: json!({ "file": dropped.hash }),
                },
            ],
            t(1),
        )
        .unwrap();
    store
        .apply(
            "mac",
            &[Op::Delete {
                kind: "session_messages".into(),
                id: "[\"m2\"]".into(),
            }],
            t(2),
        )
        .unwrap();

    // Within the grace period nothing goes.
    let now = t(100_000) + blobs::GRACE - chrono::Duration::seconds(1);
    let gone = blobs::collect(&store, &mut blobs, t(1000), blobs::GRACE).unwrap();
    assert!(gone.is_empty());

    // After it, only unreferenced files older than the grace period go.
    let mut gone: Vec<String> = blobs::collect(&store, &mut blobs, now, blobs::GRACE)
        .unwrap()
        .into_iter()
        .map(|m| m.hash)
        .collect();
    gone.sort();
    let mut want = vec![orphan.hash.clone(), dropped.hash.clone()];
    want.sort();
    assert_eq!(gone, want);
    assert!(blobs.stat(&kept.hash).unwrap().is_some());
    assert!(blobs.stat(&fresh.hash).unwrap().is_some());
    assert!(blobs.stat(&orphan.hash).unwrap().is_none());
    // Running again finds nothing more.
    assert!(
        blobs::collect(&store, &mut blobs, now, blobs::GRACE)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn file_cleanup_in_memory() {
    file_cleanup(MemStore::new());
}

#[test]
fn file_cleanup_on_sqlite() {
    file_cleanup(SqliteStore::in_memory().unwrap());
}

#[test]
fn file_cleanup_pages_through_many_files() {
    use tab_store::blob::{BlobStore, MemBlobStore};
    let store = MemStore::new();
    let mut blobs = MemBlobStore::new();
    for i in 0..1200 {
        blobs.put(format!("f{i}").as_bytes(), t(0)).unwrap();
    }
    let gone = blobs::collect(
        &store,
        &mut blobs,
        t(0) + chrono::Duration::days(2),
        blobs::GRACE,
    )
    .unwrap();
    assert_eq!(gone.len(), 1200);
    assert_eq!(blobs.stats().unwrap().files, 0);
}

#[test]
fn importing_moves_a_hub_over_the_wire() {
    // The desktop's hub, with some history.
    let mut local = SqliteStore::in_memory().unwrap();
    history(&mut local);
    let log = local.changes_since(0, 100).unwrap();
    let page = |from: usize, to: usize| {
        Req::Import(pb::Import {
            changes: log[from..to].iter().map(Into::into).collect(),
        })
    };

    let mut cloud = Hub::new(SqliteStore::in_memory().unwrap(), "cloud", t(0));
    cloud.connect(MAC, "", t(0));
    cloud.connect(PHONE, "", t(0));
    send(&mut cloud, MAC, 1, hello("mac", 0), t(9));
    send(&mut cloud, PHONE, 1, hello("phone", 0), t(9));

    // A page that doesn't start at 1 is refused.
    let h = send(&mut cloud, MAC, 2, page(1, 3), t(9));
    assert!(matches!(body(&h), Res::Error(e) if e.code == pb::error::Code::Conflict as i32));

    // Two pages, in order, keep every seq, author and time, and are pushed
    // to the phone like any write.
    let h = send(&mut cloud, MAC, 3, page(0, 2), t(9));
    assert!(matches!(body(&h), Res::Imported(i) if i.head == 2), "{h:?}");
    assert!(
        matches!(&h.push, Some(p) if matches!(&p.body, Some(Res::Changes(c)) if c.changes.len() == 2))
    );
    // Only the importing connection may carry on.
    let h = send(&mut cloud, PHONE, 2, page(2, log.len()), t(9));
    assert!(matches!(body(&h), Res::Error(e) if e.code == pb::error::Code::Conflict as i32));
    // Skipping ahead is refused, and writes nothing.
    let h = send(&mut cloud, MAC, 4, page(3, log.len()), t(9));
    assert!(
        matches!(body(&h), Res::Error(e) if e.message.contains("expected change 3")),
        "{h:?}"
    );
    let h = send(&mut cloud, MAC, 5, page(2, log.len()), t(9));
    assert!(matches!(body(&h), Res::Imported(i) if i.head == log.len() as u64));
    assert_eq!(cloud.store().changes_since(0, 100).unwrap(), log);
    assert_eq!(
        cloud.store().search("three", 5).unwrap(),
        local.search("three", 5).unwrap()
    );

    // A hub that's in use can't be imported into, even by a new connection
    // from the same device.
    cloud.connect(3, "", t(10));
    send(&mut cloud, 3, 1, hello("mac", 0), t(10));
    let h = send(&mut cloud, 3, 2, page(0, 1), t(10));
    assert!(matches!(body(&h), Res::Error(e) if e.message.contains("already has")));
}
