use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tab_protocol::pb::server_envelope::Body as Res;
use tab_protocol::{ClientEnvelope, decode_server};
use tokio_tungstenite::tungstenite::Message as WsMessage;

use super::*;
use crate::model::{IncomingEvent, ItemState, QueueItem};
use crate::store::Upsert;

fn temp_hub(name: &str) -> (Arc<LocalHub>, PathBuf) {
    let dir = std::env::temp_dir().join(format!("tab-local-hub-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("hub.db");
    (LocalHub::open(&path).unwrap(), dir)
}

fn when() -> chrono::DateTime<Utc> {
    chrono::DateTime::from_timestamp(1_800_000_000, 0).unwrap()
}

fn add(store: &Store, id: &str, title: &str) -> QueueItem {
    let e = IncomingEvent {
        source: "github".into(),
        external_id: id.into(),
        kind: "github.review_requested".into(),
        title: title.into(),
        occurred_at: when(),
        ..Default::default()
    };
    match store.upsert(&e, when()).unwrap() {
        Upsert::Inserted(i) | Upsert::Updated(i) => i,
        Upsert::Unchanged => panic!("expected a write"),
    }
}

fn hub_item(hub: &LocalHub, id: &str) -> Option<serde_json::Value> {
    hub.hub
        .lock()
        .unwrap()
        .store()
        .get("items", &json!([id]).to_string())
        .unwrap()
        .map(|r| r.body)
}

/// Waits (briefly) for the background sync to get somewhere.
async fn until(what: &str, mut f: impl FnMut() -> bool) {
    for _ in 0..200 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("timed out waiting for {what}");
}

#[tokio::test]
async fn existing_data_is_copied_in_and_new_changes_follow() {
    let (hub, dir) = temp_hub("follow");
    let store = Arc::new(Store::in_memory().unwrap());
    let before = add(&store, "1", "Already here");
    let (events, _) = broadcast::channel(16);
    hub.follow(store.clone(), events);
    until("the backfill", || hub_item(&hub, &before.id).is_some()).await;
    assert_eq!(hub_item(&hub, &before.id).unwrap()["title"], "Already here");

    store
        .set_state(&before.id, ItemState::Done, None, when())
        .unwrap();
    until("the state change", || {
        hub_item(&hub, &before.id).is_some_and(|b| b["state"] == "done")
    })
    .await;
    let after = add(&store, "2", "New one");
    until("the new item", || hub_item(&hub, &after.id).is_some()).await;

    let st = hub.status().unwrap();
    assert_eq!(st.pending, 0);
    assert!(st.records >= 2);
    // The app is its own connection, caught up.
    assert_eq!(st.connections.len(), 1);
    assert!(st.connections[0].this_app);
    assert_eq!(st.connections[0].device, "this-computer");
    assert_eq!(st.connections[0].behind, 0);
    assert_eq!(st.runs_on, "local");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn repeated_writes_of_the_same_row_are_sent_once() {
    let (hub, dir) = temp_hub("dedupe");
    let store = Arc::new(Store::in_memory().unwrap());
    store.hub_sync_install().unwrap();
    store.hub_backfill_once().unwrap();
    let it = add(&store, "1", "x");
    for _ in 0..5 {
        store
            .set_state(&it.id, ItemState::Snoozed, None, when())
            .unwrap();
    }
    hub.drain_once(&store).unwrap();
    // One put for the item; nothing else in the outbox touched items.
    let head = hub.status().unwrap().head;
    hub.drain_once(&store).unwrap();
    assert_eq!(hub.status().unwrap().head, head);
    assert_eq!(store.hub_outbox_len().unwrap(), 0);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn status_reports_this_process() {
    let (hub, dir) = temp_hub("resources");
    let st = hub.status().unwrap();
    assert!(st.cpus >= 1);
    assert!(st.memory_bytes.unwrap_or(0) > 0, "{st:?}");
    assert!(st.path.ends_with("hub.db"));
    assert!(st.storage_bytes > 0);
    let _ = std::fs::remove_dir_all(dir);
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn next(ws: &mut Socket) -> tab_protocol::ServerEnvelope {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("the hub should answer")
            .unwrap()
            .unwrap();
        if let WsMessage::Binary(b) = msg {
            return decode_server(&b).unwrap();
        }
    }
}

fn frame(id: u64, req: Req) -> WsMessage {
    WsMessage::Binary(encode(&ClientEnvelope::new(id, req)).into())
}

#[tokio::test]
async fn another_device_syncs_both_ways() {
    let (hub, dir) = temp_hub("ws");
    let store = Arc::new(Store::in_memory().unwrap());
    let it = add(&store, "1", "Review login");
    let (events, mut ui) = broadcast::channel(16);
    hub.follow(store.clone(), events);
    until("the backfill", || hub_item(&hub, &it.id).is_some()).await;
    let url = hub.serve(48_700, "secret".into()).await.unwrap();

    // No token, no connection.
    assert!(tokio_tungstenite::connect_async(url.clone()).await.is_err());
    assert!(
        tokio_tungstenite::connect_async(format!("{url}?token=wrong"))
            .await
            .is_err()
    );

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("{url}?token=secret"))
        .await
        .unwrap();
    ws.send(frame(
        1,
        Req::Hello(pb::Hello {
            device: "phone".into(),
            client: "test".into(),
            since: 0,
        }),
    ))
    .await
    .unwrap();
    let welcome = next(&mut ws).await;
    assert_eq!(welcome.request_id, 1);
    let head = match welcome.body {
        Some(Res::Welcome(w)) => w.head,
        other => panic!("{other:?}"),
    };
    assert!(head > 0);

    // The phone reads the item and marks it done.
    ws.send(frame(
        2,
        Req::GetRecord(pb::GetRecord {
            kind: "items".into(),
            id: json!([it.id]).to_string(),
        }),
    ))
    .await
    .unwrap();
    let Some(Res::Record(pb::RecordReply { record: Some(rec) })) = next(&mut ws).await.body else {
        panic!("no record")
    };
    let mut row: serde_json::Value = serde_json::from_slice(&rec.body_json).unwrap();
    row["state"] = json!("done");
    let put = Op::Put {
        kind: "items".into(),
        id: json!([it.id]).to_string(),
        body: row,
    };
    ws.send(frame(
        3,
        Req::Apply(pb::Apply {
            ops: vec![(&put).into()],
        }),
    ))
    .await
    .unwrap();
    assert!(matches!(next(&mut ws).await.body, Some(Res::Applied(_))));
    // It lands in tab.db and the UI hears about it.
    assert_eq!(store.get(&it.id).unwrap().unwrap().state, ItemState::Done);
    let ev = tokio::time::timeout(Duration::from_secs(2), ui.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(ev, EngineEvent::ItemChanged { item, .. } if item.state == ItemState::Done));
    // ...without echoing back out.
    assert_eq!(store.hub_outbox_len().unwrap(), 0);

    // A change on the desktop is pushed to the phone.
    add(&store, "2", "From the desktop");
    let push = next(&mut ws).await;
    assert_eq!(push.request_id, 0);
    assert!(matches!(push.body, Some(Res::Changes(c)) if !c.changes.is_empty()));

    let st = hub.status().unwrap();
    let phone = st.connections.iter().find(|c| c.device == "phone").unwrap();
    assert!(phone.address.starts_with("127.0.0.1:"));
    assert_eq!(st.url.as_deref(), Some(url.as_str()));

    drop(ws);
    until("the phone to drop off", || {
        hub.status().unwrap().connections.len() == 1
    })
    .await;
    let _ = std::fs::remove_dir_all(dir);
}
