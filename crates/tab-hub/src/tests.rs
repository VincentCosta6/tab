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
