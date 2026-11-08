use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tab_protocol::pb::server_envelope::Body as Res;
use tab_protocol::{ClientEnvelope, decode_server};
use tokio_tungstenite::tungstenite::Message as WsMessage;

use super::*;
use crate::live::SteerCommand;
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
    assert_eq!(st.connections[0].device, store.device_id().unwrap());
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
    hub.push_once(&store).unwrap();
    // One put for the item; nothing else in the outbox touched items.
    let head = hub.status().unwrap().head;
    hub.push_once(&store).unwrap();
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

    assert!(url.starts_with("wss://127.0.0.1:"));
    // No token, no connection; a token in the URL doesn't count.
    assert!(ws_connect(&hub, &url, None).await.is_err());
    assert!(
        ws_connect(&hub, &format!("{url}?token=secret"), None)
            .await
            .is_err()
    );
    assert!(ws_connect(&hub, &url, Some("wrong")).await.is_err());
    // Plain ws:// isn't served at all.
    assert!(
        tokio_tungstenite::connect_async(url.replace("wss://", "ws://"))
            .await
            .is_err()
    );

    let mut ws = ws_connect(&hub, &url, Some("secret")).await.unwrap();
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
    // It lands in tab.db (sync runs on a blocking thread, so shortly) and
    // the UI hears about it.
    until("the phone's change", || {
        store.get(&it.id).unwrap().unwrap().state == ItemState::Done
    })
    .await;
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

fn title_in(store: &Store, id: &str) -> Option<String> {
    store.get(id).unwrap().map(|i| i.event.title)
}

#[test]
fn a_fresh_tab_db_is_rebuilt_from_the_hub() {
    let (hub, dir) = temp_hub("rebuild");
    let first = Store::in_memory().unwrap();
    hub.introduce(&first).unwrap();
    let a = add(&first, "1", "One");
    let b = add(&first, "2", "Two");
    first
        .set_state(&b.id, ItemState::Done, None, when())
        .unwrap();
    hub.sync(&first).unwrap();

    // tab.db lost (or a new install pointed at the same hub).
    let fresh = Store::in_memory().unwrap();
    hub.introduce(&fresh).unwrap();
    hub.sync(&fresh).unwrap();
    assert_eq!(title_in(&fresh, &a.id).as_deref(), Some("One"));
    assert_eq!(fresh.get(&b.id).unwrap().unwrap().state, ItemState::Done);
    // Rebuilding didn't send anything back.
    assert_eq!(fresh.hub_outbox_len().unwrap(), 0);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn joining_a_hub_with_data_takes_its_rows_and_adds_the_rest() {
    let (hub, dir) = temp_hub("join");
    let first = Store::in_memory().unwrap();
    hub.introduce(&first).unwrap();
    let shared = add(&first, "1", "Hub's title");
    hub.sync(&first).unwrap();

    // The same notification, ingested here under its own id.
    let other = Store::in_memory().unwrap();
    let twin = add(&other, "1", "Local title");
    assert_ne!(twin.id, shared.id);
    let only_here = add(&other, "9", "Only here");
    hub.introduce(&other).unwrap();
    hub.sync(&other).unwrap();

    assert_eq!(title_in(&other, &shared.id).as_deref(), Some("Hub's title"));
    assert_eq!(title_in(&other, &twin.id), None);
    assert_eq!(hub_item(&hub, &shared.id).unwrap()["title"], "Hub's title");
    assert_eq!(hub_item(&hub, &twin.id), None);
    assert_eq!(hub_item(&hub, &only_here.id).unwrap()["title"], "Only here");
    assert_eq!(other.hub_outbox_len().unwrap(), 0);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_replaced_hub_gets_everything_again() {
    let (old, dir) = temp_hub("replaced-old");
    let store = Store::in_memory().unwrap();
    old.introduce(&store).unwrap();
    let a = add(&store, "1", "Keep me");
    old.sync(&store).unwrap();
    assert!(store.hub_pulled_to().unwrap() > 0);

    let (new, dir2) = temp_hub("replaced-new");
    new.introduce(&store).unwrap();
    new.sync(&store).unwrap();
    assert_eq!(hub_item(&new, &a.id).unwrap()["title"], "Keep me");
    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::remove_dir_all(dir2);
}

#[test]
fn a_change_the_hub_refuses_is_undone_here() {
    let (hub, dir) = temp_hub("refused");
    let store = Store::in_memory().unwrap();
    hub.introduce(&store).unwrap();
    let a = add(&store, "1", "Fine");
    let b = add(&store, "2", "Also fine");
    hub.sync(&store).unwrap();

    // Over the hub's size limit for one record.
    let huge = "x".repeat(tab_domain::ops::MAX_BODY_BYTES + 1);
    store
        .item_set_title(&a.id, &huge, &json!({}), when())
        .unwrap();
    store
        .set_state(&b.id, ItemState::Done, None, when())
        .unwrap();
    hub.sync(&store).unwrap();

    assert_eq!(title_in(&store, &a.id).as_deref(), Some("Fine"));
    assert_eq!(hub_item(&hub, &a.id).unwrap()["title"], "Fine");
    // The rest of the batch still went through.
    assert_eq!(hub_item(&hub, &b.id).unwrap()["state"], "done");
    assert_eq!(store.hub_outbox_len().unwrap(), 0);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_same_item_ingested_on_two_devices_ends_up_once() {
    let (hub, dir) = temp_hub("twins");
    let mac = Store::in_memory().unwrap();
    let pc = Store::in_memory().unwrap();
    hub.introduce(&mac).unwrap();
    hub.introduce(&pc).unwrap();
    let first = add(&mac, "42", "From the Mac");
    hub.sync(&mac).unwrap();
    let second = add(&pc, "42", "From the PC");
    hub.sync(&pc).unwrap();
    hub.sync(&mac).unwrap();
    hub.sync(&pc).unwrap();

    // Whichever the hub ordered last wins, and both devices agree.
    let on_mac: Vec<_> = [&first.id, &second.id]
        .iter()
        .filter_map(|id| mac.get(id).unwrap())
        .map(|i| (i.id, i.event.title))
        .collect();
    let on_pc: Vec<_> = [&first.id, &second.id]
        .iter()
        .filter_map(|id| pc.get(id).unwrap())
        .map(|i| (i.id, i.event.title))
        .collect();
    assert_eq!(on_mac.len(), 1, "{on_mac:?}");
    assert_eq!(on_mac, on_pc);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn changes_reach_the_hub_as_they_commit() {
    let (hub, dir) = temp_hub("prompt");
    let store = Arc::new(Store::in_memory().unwrap());
    let (events, _) = broadcast::channel(16);
    hub.follow(store.clone(), events);
    until("setup", || store.hub_backfilled().unwrap_or(false)).await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    let started = std::time::Instant::now();
    let it = add(&store, "1", "Quick");
    while hub_item(&hub, &it.id).is_none() {
        assert!(started.elapsed() < IDLE / 2, "waited for the idle backstop");
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    let _ = std::fs::remove_dir_all(dir);
}

// ---- Files and the live channel ----

#[test]
fn attached_files_go_into_the_hub_and_back_out() {
    let (hub, dir) = temp_hub("files");
    let cache = dir.join("attachments");
    let path = hub.attach(&cache, b"png bytes", "../shot.png").unwrap();
    assert!(path.starts_with(&cache));
    assert_eq!(std::fs::read(&path).unwrap(), b"png bytes");
    let file = crate::live::file_ref(&path).unwrap();
    assert_eq!(file.hash, tab_domain::blob::hash(b"png bytes"));
    assert_eq!(file.name, "shot.png");

    // The cache can be swept; the hub still has it.
    std::fs::remove_dir_all(&cache).unwrap();
    let again = hub
        .file_materialize(&cache, &file.hash, &file.name)
        .unwrap();
    assert_eq!(again, path);
    assert_eq!(std::fs::read(&again).unwrap(), b"png bytes");
    assert!(
        hub.file_materialize(&cache, &tab_domain::blob::hash(b"never"), "x")
            .is_err()
    );

    let st = hub.status().unwrap();
    assert_eq!((st.files, st.file_bytes), (1, 9));
    // Fresh and unreferenced: kept for the grace period.
    assert_eq!(hub.collect_files().unwrap(), 0);
    let _ = std::fs::remove_dir_all(dir);
}

/// Opens the hub's WebSocket the way a paired device does: TLS 1.3 with the
/// hub's certificate pinned, and the token in a header.
async fn ws_connect(
    hub: &LocalHub,
    url: &str,
    token: Option<&str>,
) -> Result<Socket, tokio_tungstenite::tungstenite::Error> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let fp = hub.status().unwrap().fingerprint.unwrap();
    let tls = crate::hub_tls::pinned_client(&fp, &[crate::hub_tls::H1]).unwrap();
    let mut req = url.into_client_request()?;
    if let Some(t) = token {
        req.headers_mut()
            .insert("authorization", format!("Bearer {t}").parse().unwrap());
    }
    tokio_tungstenite::connect_async_tls_with_config(
        req,
        None,
        false,
        Some(tokio_tungstenite::Connector::Rustls(Arc::new(tls))),
    )
    .await
    .map(|(ws, _)| ws)
}

async fn connect(hub: &LocalHub, url: &str, device: &str) -> Socket {
    let mut ws = ws_connect(hub, url, Some("secret")).await.unwrap();
    ws.send(frame(
        1,
        Req::Hello(pb::Hello {
            device: device.into(),
            client: "test".into(),
            since: 0,
        }),
    ))
    .await
    .unwrap();
    assert!(matches!(next(&mut ws).await.body, Some(Res::Welcome(_))));
    ws
}

#[tokio::test]
async fn files_upload_and_download_over_http2_and_http3() {
    let (hub, dir) = temp_hub("http");
    let url = hub.serve(48_740, "secret".into()).await.unwrap();
    let st = hub.status().unwrap();
    assert!(st.http3, "HTTP/3 should be up");
    let base = url.replace("wss://", "https://").replace("/hub", "");
    let fp = st.fingerprint.unwrap();

    // HTTP/2 over TLS 1.3, certificate pinned.
    let tls = crate::hub_tls::pinned_client(&fp, &[crate::hub_tls::H2]).unwrap();
    let client = reqwest::Client::builder()
        .use_preconfigured_tls(tls)
        .build()
        .unwrap();
    let res = client
        .put(format!("{base}/files"))
        .body("x")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 401);
    assert_eq!(res.version(), reqwest::Version::HTTP_2);
    let res = client
        .put(format!("{base}/files?token=secret"))
        .body("x")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 401, "a token in the URL doesn't count");
    let res = client
        .put(format!("{base}/files"))
        .bearer_auth("secret")
        .body("a file from the phone")
        .send()
        .await
        .unwrap();
    // HTTP/3 is advertised.
    let port = url.rsplit(':').next().unwrap().trim_end_matches("/hub");
    assert_eq!(
        res.headers()["alt-svc"].to_str().unwrap(),
        format!("h3=\":{port}\"; ma=86400")
    );
    let res: serde_json::Value = res.json().await.unwrap();
    let hash = res["hash"].as_str().unwrap().to_string();
    assert_eq!(hash, tab_domain::blob::hash(b"a file from the phone"));
    assert_eq!(res["size"], 21);
    let got = client
        .get(format!("{base}/files/{hash}"))
        .bearer_auth("secret")
        .send()
        .await
        .unwrap();
    assert_eq!(got.status(), 200);
    assert_eq!(&got.bytes().await.unwrap()[..], b"a file from the phone");
    let missing = client
        .get(format!("{base}/files/{}", tab_domain::blob::hash(b"nope")))
        .bearer_auth("secret")
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), 404);

    // A client that pins another certificate won't talk to it.
    let other =
        crate::hub_tls::pinned_client(&format!("sha256/{}", "0".repeat(64)), &[crate::hub_tls::H2])
            .unwrap();
    let wrong = reqwest::Client::builder()
        .use_preconfigured_tls(other)
        .build()
        .unwrap();
    assert!(
        wrong
            .get(format!("{base}/files/{hash}"))
            .send()
            .await
            .is_err()
    );

    // TLS 1.2 is refused.
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(rustls::pki_types::CertificateDer::from(
            std::fs::read(dir.join("hub-tls/cert.der")).unwrap(),
        ))
        .unwrap();
    let tls12 = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS12])
    .unwrap()
    .with_root_certificates(roots)
    .with_no_client_auth();
    let old = reqwest::Client::builder()
        .use_preconfigured_tls(tls12)
        .build()
        .unwrap();
    let e = old
        .get(format!("{base}/files/{hash}"))
        .send()
        .await
        .unwrap_err();
    assert!(
        format!("{e:?}").contains("AlertReceived(ProtocolVersion)"),
        "{e:?}"
    );

    // HTTP/3 over QUIC on the same port.
    let addr: SocketAddr = base.trim_start_matches("https://").parse().unwrap();
    let (status, body) = h3_request(
        &fp,
        addr,
        "GET",
        &format!("/files/{hash}"),
        Some("secret"),
        vec![],
    )
    .await;
    assert_eq!((status, &body[..]), (200, &b"a file from the phone"[..]));
    let (status, body) = h3_request(
        &fp,
        addr,
        "PUT",
        "/files",
        Some("secret"),
        b"over quic".to_vec(),
    )
    .await;
    assert_eq!(status, 200);
    let put: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(put["hash"], tab_domain::blob::hash(b"over quic"));
    assert_eq!(
        hub.file_get(put["hash"].as_str().unwrap())
            .unwrap()
            .unwrap(),
        b"over quic"
    );
    let (status, _) = h3_request(&fp, addr, "GET", &format!("/files/{hash}"), None, vec![]).await;
    assert_eq!(status, 401);
    let _ = std::fs::remove_dir_all(dir);
}

/// One HTTP/3 request to the hub, its certificate pinned.
async fn h3_request(
    fp: &str,
    addr: SocketAddr,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Vec<u8>,
) -> (u16, Vec<u8>) {
    use bytes::Buf;
    let tls = crate::hub_tls::pinned_client(fp, &[crate::hub_tls::H3]).unwrap();
    let crypto = quinn::crypto::rustls::QuicClientConfig::try_from(tls).unwrap();
    let mut endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    endpoint.set_default_client_config(quinn::ClientConfig::new(Arc::new(crypto)));
    let conn = endpoint.connect(addr, "localhost").unwrap().await.unwrap();
    let (mut driver, mut send) = h3::client::new(h3_quinn::Connection::new(conn))
        .await
        .unwrap();
    let drive = tokio::spawn(async move { std::future::poll_fn(|cx| driver.poll_close(cx)).await });
    let mut req = http::Request::builder()
        .method(method)
        .uri(format!("https://localhost{path}"));
    if let Some(t) = token {
        req = req.header("authorization", format!("Bearer {t}"));
    }
    let mut stream = send.send_request(req.body(()).unwrap()).await.unwrap();
    if !body.is_empty() {
        stream.send_data(bytes::Bytes::from(body)).await.unwrap();
    }
    stream.finish().await.unwrap();
    let res = stream.recv_response().await.unwrap();
    let mut out = vec![];
    while let Some(mut chunk) = stream.recv_data().await.unwrap() {
        out.extend_from_slice(&chunk.copy_to_bytes(chunk.remaining()));
    }
    drop(stream);
    drop(send);
    drive.abort();
    (res.status().as_u16(), out)
}

#[tokio::test]
async fn this_app_watches_and_steers_a_session_another_device_runs() {
    let (hub, dir) = temp_hub("live");
    let store = Arc::new(Store::in_memory().unwrap());
    let (events, mut ui) = broadcast::channel(64);
    hub.follow(store.clone(), events);
    let url = hub.serve(48_760, "secret".into()).await.unwrap();
    let mut laptop = connect(&hub, &url, "device-laptop").await;

    // The laptop's chat syncs in, mid-turn.
    let sid = "s-laptop";
    let session = Op::Put {
        kind: "sessions".into(),
        id: json!([sid]).to_string(),
        body: json!({
            "id": sid, "item_id": null, "agent": "claude_code", "agent_session_id": null,
            "title": "On the laptop", "status": "working", "cwd": "/w",
            "created_at": "2027-01-15T08:00:00.000Z", "updated_at": "2027-01-15T08:00:00.000Z",
            "key": null, "brief": null, "device": "device-laptop",
        }),
    };
    laptop
        .send(frame(
            2,
            Req::Apply(pb::Apply {
                ops: vec![(&session).into()],
            }),
        ))
        .await
        .unwrap();
    assert!(matches!(
        next(&mut laptop).await.body,
        Some(Res::Applied(_))
    ));
    until("the chat to land here", || {
        store.session_get(sid).unwrap().is_some()
    })
    .await;
    // The app starts watching it: the laptop is told.
    let watched = loop {
        if let Some(Res::Watched(w)) = next(&mut laptop).await.body {
            break w;
        }
    };
    assert_eq!((watched.session.as_str(), watched.watchers), (sid, 1));

    // What the laptop streams shows up here.
    let delta = crate::live::LiveEvent::SessionDelta {
        session_id: sid.into(),
        text: "Looking at the".into(),
    };
    let approval = crate::live::LiveEvent::ApprovalRequested {
        approval: crate::model::Approval {
            id: "a1".into(),
            session_id: sid.into(),
            tool: "Bash".into(),
            summary: "rm -rf build".into(),
            input: json!({}),
        },
    };
    for (i, ev) in [&delta, &approval].into_iter().enumerate() {
        laptop
            .send(frame(
                10 + i as u64,
                Req::Publish(pb::Publish {
                    session: sid.into(),
                    event_json: serde_json::to_vec(ev).unwrap(),
                }),
            ))
            .await
            .unwrap();
    }
    let mut seen = vec![];
    while seen.len() < 2 {
        let ev = tokio::time::timeout(Duration::from_secs(2), ui.recv())
            .await
            .expect("live events should reach the UI")
            .unwrap();
        match ev {
            EngineEvent::SessionDelta { session_id, text } => {
                assert_eq!(
                    (session_id.as_str(), text.as_str()),
                    (sid, "Looking at the")
                );
                seen.push("delta");
            }
            EngineEvent::ApprovalRequested { approval } => {
                assert_eq!(approval.id, "a1");
                seen.push("approval");
            }
            _ => {}
        }
    }
    assert_eq!(hub.remote_approval("a1").as_deref(), Some(sid));

    // Answering here, and a message with a file, go to the laptop.
    hub.steer_remote(
        sid,
        &SteerCommand::Approval {
            id: "a1".into(),
            decision: "allow".into(),
            message: None,
            answers: None,
        },
    )
    .unwrap();
    let file = hub.file_put(b"diagram").unwrap();
    hub.steer_remote(
        sid,
        &SteerCommand::Message {
            text: "use this".into(),
            files: vec![crate::live::FileRef {
                hash: file.hash.clone(),
                name: "d.png".into(),
                size: file.size,
                path: None,
            }],
        },
    )
    .unwrap();
    let mut got = vec![];
    while got.len() < 2 {
        if let Some(Res::Steered(s)) = next(&mut laptop).await.body {
            assert_eq!(s.session, sid);
            got.push(serde_json::from_slice::<SteerCommand>(&s.body_json).unwrap());
        }
    }
    assert!(matches!(&got[0], SteerCommand::Approval { id, .. } if id == "a1"));
    assert!(matches!(&got[1], SteerCommand::Message { files, .. } if files[0].hash == file.hash));

    // The turn ends: the app stops watching.
    let mut done = session.clone();
    if let Op::Put { body, .. } = &mut done {
        body["status"] = json!("idle");
    }
    laptop
        .send(frame(
            3,
            Req::Apply(pb::Apply {
                ops: vec![(&done).into()],
            }),
        ))
        .await
        .unwrap();
    let watched = loop {
        if let Some(Res::Watched(w)) = next(&mut laptop).await.body {
            break w;
        }
    };
    assert_eq!(watched.watchers, 0);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn steering_a_session_whose_device_is_away_says_so() {
    let (hub, dir) = temp_hub("away");
    let store = Arc::new(Store::in_memory().unwrap());
    let (events, _) = broadcast::channel(16);
    hub.follow(store.clone(), events);
    hub.apply(&[Op::Put {
        kind: "sessions".into(),
        id: json!(["s1"]).to_string(),
        body: json!({ "id": "s1", "device": "device-desktop-at-home" }),
    }])
    .unwrap();
    let e = hub.steer_remote("s1", &SteerCommand::Stop).unwrap_err();
    assert!(e.to_string().contains("isn't connected"), "{e}");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_phone_watches_a_session_this_app_runs_and_answers_its_prompt() {
    let (hub, dir) = temp_hub("runner");
    let store = Arc::new(Store::in_memory().unwrap());
    let (events, _) = broadcast::channel(64);
    let agents = crate::agents::AgentHub::new(
        store.clone(),
        events.clone(),
        crate::config::AgentConfig {
            kind: Some(crate::model::AgentKind::ClaudeCode),
            ..Default::default()
        },
        dir.join("agents"),
    );
    hub.attach_agents(agents.clone());
    hub.follow(store.clone(), events.clone());
    let url = hub.serve(48_780, "secret".into()).await.unwrap();
    let session = store
        .session_create(
            None,
            crate::model::AgentKind::ClaudeCode,
            "Here",
            "/w",
            Utc::now(),
        )
        .unwrap();
    until("the chat to reach the hub", || {
        hub.hub.lock().unwrap().runner(&session.id).is_some()
    })
    .await;

    // The agent is part-way through a reply when the phone starts watching.
    let _ = events.send(EngineEvent::SessionDelta {
        session_id: session.id.clone(),
        text: "So far ".into(),
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    let mut phone = connect(&hub, &url, "phone").await;
    phone
        .send(frame(
            2,
            Req::Watch(pb::Watch {
                sessions: vec![session.id.clone()],
            }),
        ))
        .await
        .unwrap();
    let live = |env: tab_protocol::ServerEnvelope| match env.body {
        Some(Res::Live(l)) => {
            Some(serde_json::from_slice::<crate::live::LiveEvent>(&l.event_json).unwrap())
        }
        _ => None,
    };
    // It gets the text so far, then what streams after.
    let first = loop {
        if let Some(ev) = live(next(&mut phone).await) {
            break ev;
        }
    };
    assert_eq!(
        first,
        crate::live::LiveEvent::SessionDelta {
            session_id: session.id.clone(),
            text: "So far ".into()
        }
    );

    // The agent asks; the phone sees the prompt and answers it.
    let (a, sid) = (agents.clone(), session.id.clone());
    let asked = tokio::spawn(async move {
        a.request_approval(&sid, "Bash", "rm -rf build".into(), json!({}))
            .await
    });
    let approval = loop {
        if let Some(crate::live::LiveEvent::ApprovalRequested { approval }) =
            live(next(&mut phone).await)
        {
            break approval;
        }
    };
    let cmd = SteerCommand::Approval {
        id: approval.id.clone(),
        decision: "allow".into(),
        message: None,
        answers: None,
    };
    phone
        .send(frame(
            3,
            Req::Steer(pb::Steer {
                session: session.id.clone(),
                body_json: serde_json::to_vec(&cmd).unwrap(),
            }),
        ))
        .await
        .unwrap();
    let decision = tokio::time::timeout(Duration::from_secs(3), asked)
        .await
        .expect("the answer should reach the agent")
        .unwrap();
    assert_eq!(decision, crate::agents::Decision::Allow);
    let _ = std::fs::remove_dir_all(dir);
}

/// A stand-in for tab cloud: tab's hub behind `/v1/hubs/{id}/sync`, and its
/// files at `/v1/hubs/{id}/files`, the way the hub Durable Object serves
/// them (minus accounts, and R2).
async fn fake_cloud() -> (String, Arc<Mutex<tab_hub::Hub<SqliteStore>>>) {
    use axum::extract::Path as UrlPath;
    type Cloud = Arc<Mutex<tab_hub::Hub<SqliteStore>>>;
    let hub: Cloud = Arc::new(Mutex::new(tab_hub::Hub::new(
        SqliteStore::in_memory().unwrap(),
        "cloud",
        Utc::now(),
    )));
    async fn sync(
        ws: WebSocketUpgrade,
        headers: axum::http::HeaderMap,
        UrlPath(_id): UrlPath<String>,
        State((hub, next)): State<(Cloud, Arc<AtomicU64>)>,
    ) -> Response {
        if headers.get("authorization").and_then(|v| v.to_str().ok()) != Some("Bearer t0k") {
            return StatusCode::UNAUTHORIZED.into_response();
        }
        ws.on_upgrade(move |mut socket| async move {
            let conn = next.fetch_add(1, Ordering::Relaxed);
            hub.lock().unwrap().connect(conn, "", Utc::now());
            while let Some(Ok(Message::Binary(b))) = socket.recv().await {
                let h = hub.lock().unwrap().handle_frame(conn, &b, Utc::now());
                if socket
                    .send(Message::Binary(encode(&h.reply).into()))
                    .await
                    .is_err()
                {
                    break;
                }
            }
            hub.lock().unwrap().disconnect(conn);
        })
    }
    type Files = Arc<Mutex<tab_store::blob::MemBlobStore>>;
    fn authed(headers: &axum::http::HeaderMap) -> bool {
        headers.get("authorization").and_then(|v| v.to_str().ok()) == Some("Bearer t0k")
    }
    async fn put_file(
        headers: axum::http::HeaderMap,
        State(files): State<Files>,
        body: axum::body::Bytes,
    ) -> Response {
        use prost::Message as _;
        if !authed(&headers) {
            return StatusCode::UNAUTHORIZED.into_response();
        }
        let m = files.lock().unwrap().put(&body, Utc::now()).unwrap();
        tab_protocol::cloud::StoredFile {
            hash: m.hash,
            size: m.size,
        }
        .encode_to_vec()
        .into_response()
    }
    async fn get_file(
        headers: axum::http::HeaderMap,
        UrlPath((_id, hex)): UrlPath<(String, String)>,
        State(files): State<Files>,
    ) -> Response {
        if !authed(&headers) {
            return StatusCode::UNAUTHORIZED.into_response();
        }
        let hash = tab_domain::blob::from_hex(&hex).unwrap_or_default();
        match files.lock().unwrap().get(&hash).unwrap() {
            Some(b) => b.into_response(),
            None => StatusCode::NOT_FOUND.into_response(),
        }
    }
    let files = Router::new()
        .route("/v1/hubs/{id}/files", axum::routing::put(put_file))
        .route("/v1/hubs/{id}/files/{hex}", get(get_file))
        .with_state(Files::default());
    let app = Router::new()
        .route("/v1/hubs/{id}/sync", get(sync))
        .with_state((hub.clone(), Arc::new(AtomicU64::new(1))))
        .merge(files);
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (format!("http://{addr}"), hub)
}

fn link_to(api: &str) -> Arc<CloudLink> {
    CloudLink::start(
        crate::cloud_link::sync_url(api, "h1"),
        "h1".into(),
        Arc::new(|| Box::pin(async { Ok("t0k".to_string()) })),
    )
}

/// Runs a blocking hub call off the runtime, as the app does.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(f).await.unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn moving_to_the_cloud_keeps_history_syncs_from_then_on_and_can_come_back() {
    let (api, cloud) = fake_cloud().await;

    // This computer, with work in its own hub.
    let (mac_hub, dir) = temp_hub("move-mac");
    let mac = Arc::new(Store::in_memory().unwrap());
    mac_hub.introduce(&mac).unwrap();
    let a = add(&mac, "1", "Before the move");
    mac_hub.sync(&mac).unwrap();
    let local_log = mac_hub
        .hub
        .lock()
        .unwrap()
        .store()
        .changes_since(0, 1000)
        .unwrap();
    assert!(!local_log.is_empty());

    // Move: the cloud gets the same log, change for change.
    let mut seen = vec![];
    let (h, s, link) = (mac_hub.clone(), mac.clone(), link_to(&api));
    let (outcome, seen) = blocking(move || {
        let o = h.move_to_cloud(link, &s, |done, total| seen.push((done, total)));
        (o, seen)
    })
    .await;
    let n = local_log.len() as u64;
    assert_eq!(outcome.unwrap(), MoveOutcome::Moved { changes: n });
    assert_eq!(seen.last(), Some(&(n, n)));
    assert_eq!(
        cloud
            .lock()
            .unwrap()
            .store()
            .changes_since(0, 1000)
            .unwrap(),
        local_log
    );

    // From now on changes go to the cloud, and hub.db keeps its copy.
    let b = add(&mac, "2", "After the move");
    let (h, s) = (mac_hub.clone(), mac.clone());
    blocking(move || h.sync(&s)).await.unwrap();
    let in_cloud = |id: &str| {
        cloud
            .lock()
            .unwrap()
            .store()
            .get("items", &json!([id]).to_string())
            .unwrap()
    };
    assert!(in_cloud(&b.id).is_some());
    assert!(hub_item(&mac_hub, &b.id).is_none());

    // A second computer that picks tab cloud joins the same hub and gets
    // everything; its changes reach the first one.
    let (pc_hub, dir2) = temp_hub("move-pc");
    let pc = Arc::new(Store::in_memory().unwrap());
    pc_hub.introduce(&pc).unwrap();
    let (h, s, link) = (pc_hub.clone(), pc.clone(), link_to(&api));
    let outcome = blocking(move || h.move_to_cloud(link, &s, |_, _| {}))
        .await
        .unwrap();
    assert!(matches!(outcome, MoveOutcome::Joined { .. }));
    assert_eq!(title_in(&pc, &a.id).as_deref(), Some("Before the move"));
    assert_eq!(title_in(&pc, &b.id).as_deref(), Some("After the move"));
    pc.set_state(&a.id, ItemState::Done, None, when()).unwrap();
    let (h, s) = (pc_hub.clone(), pc.clone());
    blocking(move || h.sync(&s)).await.unwrap();
    let (h, s) = (mac_hub.clone(), mac.clone());
    blocking(move || h.sync(&s)).await.unwrap();
    assert_eq!(mac.get(&a.id).unwrap().unwrap().state, ItemState::Done);

    // Moving again is refused.
    let (h, s, link) = (mac_hub.clone(), mac.clone(), link_to(&api));
    assert!(
        blocking(move || h.move_to_cloud(link, &s, |_, _| {}))
            .await
            .is_err()
    );

    // Moving back: hub.db gets what changed in the cloud meanwhile, and
    // from then on changes stay on this computer.
    let (h, s) = (mac_hub.clone(), mac.clone());
    blocking(move || h.move_to_local(&s)).await.unwrap();
    assert!(mac_hub.cloud_link().is_none());
    assert!(hub_item(&mac_hub, &b.id).is_some());
    assert_eq!(
        hub_item(&mac_hub, &a.id).unwrap()["state"],
        json!("done"),
        "the newer change from the cloud wins over hub.db's older one"
    );
    let c = add(&mac, "3", "Back home");
    let (h, s) = (mac_hub.clone(), mac.clone());
    blocking(move || h.sync(&s)).await.unwrap();
    assert!(hub_item(&mac_hub, &c.id).is_some());
    assert!(in_cloud(&c.id).is_none());
    let (h, s) = (mac_hub.clone(), mac.clone());
    assert!(
        blocking(move || h.move_to_local(&s)).await.is_err(),
        "already local"
    );
    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::remove_dir_all(dir2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn files_reach_other_computers_through_the_cloud() {
    let (api, _cloud) = fake_cloud().await;
    let (mac_hub, dir) = temp_hub("files-mac");
    let mac = Arc::new(Store::in_memory().unwrap());
    mac_hub.introduce(&mac).unwrap();
    let (h, s, link) = (mac_hub.clone(), mac.clone(), link_to(&api));
    blocking(move || h.move_to_cloud(link, &s, |_, _| {}).map(|_| ()))
        .await
        .unwrap();

    // The agent on this computer makes a screenshot and names it.
    let shot = dir.join("work").join("shot.png");
    std::fs::create_dir_all(shot.parent().unwrap()).unwrap();
    std::fs::write(&shot, b"png from the mac").unwrap();
    let agents = crate::agents::AgentHub::new(
        mac.clone(),
        broadcast::channel(16).0,
        Default::default(),
        dir.join("agents"),
    );
    mac_hub.attach_agents(agents.clone());
    let chat = mac
        .session_create(
            None,
            crate::model::AgentKind::ClaudeCode,
            "Shots",
            "/w",
            Utc::now(),
        )
        .unwrap();
    let text = format!("Here: `{}`", shot.display());
    agents.post(
        &chat.id,
        "assistant",
        "text",
        &text,
        serde_json::Value::Null,
    );
    let hash = tab_domain::blob::hash(b"png from the mac");
    let (h, s) = (mac_hub.clone(), mac.clone());
    blocking(move || h.sync(&s)).await.unwrap();

    // Another computer gets the chat, and the file behind the path.
    let (pc_hub, dir2) = temp_hub("files-pc");
    let pc = Arc::new(Store::in_memory().unwrap());
    pc_hub.introduce(&pc).unwrap();
    let (h, s, link) = (pc_hub.clone(), pc.clone(), link_to(&api));
    blocking(move || h.move_to_cloud(link, &s, |_, _| {}).map(|_| ()))
        .await
        .unwrap();
    let path = shot.display().to_string();
    let file = pc
        .named_file(&path)
        .unwrap()
        .expect("the reply recorded the file");
    assert_eq!(file.hash, hash);
    let (h, d, f) = (pc_hub.clone(), dir2.join("attachments"), file.clone());
    let local = blocking(move || h.file_materialize(&d, &f.hash, &f.name))
        .await
        .unwrap();
    assert_eq!(std::fs::read(local).unwrap(), b"png from the mac");

    // A file the cloud doesn't have fails rather than hangs.
    let (h, d) = (pc_hub.clone(), dir2.join("attachments"));
    let missing = tab_domain::blob::hash(b"nowhere");
    assert!(
        blocking(move || h.file_materialize(&d, &missing, "x.png"))
            .await
            .is_err()
    );
    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::remove_dir_all(dir2);
}
