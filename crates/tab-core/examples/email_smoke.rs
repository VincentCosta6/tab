//! Runs the email connector against an IMAP server and prints what it emits.
//! `TAB_IMAP_CA=ca.pem cargo run -p tab-core --example email_smoke -- host port user pass`

use std::sync::Arc;
use std::time::Duration;

use tab_core::config::ConnectorConfig;
use tab_core::connector::ConnectorFactory;
use tab_core::connectors::EmailFactory;
use tab_core::{Gateway, Store};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber_init();
    let a: Vec<String> = std::env::args().skip(1).collect();
    let mut cfg = ConnectorConfig::on();
    for (k, v) in [("host", &a[0]), ("username", &a[2]), ("password", &a[3])] {
        cfg.settings.insert(k.into(), v.clone().into());
    }
    cfg.settings
        .insert("port".into(), a[1].parse::<u64>()?.into());
    let store = Arc::new(Store::in_memory()?);
    let conn = EmailFactory
        .build(&cfg, Arc::new(Gateway::new(store)))
        .unwrap()?;
    let (ctx, mut rx, cancel) = tab_core::connector::test_context("email");
    let task = tokio::spawn(async move { conn.run(ctx).await });
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while let Ok(Some(msg)) = tokio::time::timeout_at(deadline, rx.recv()).await {
        println!("{msg}");
    }
    cancel.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(3), task).await;
    Ok(())
}

fn tracing_subscriber_init() {}
