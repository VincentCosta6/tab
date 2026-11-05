//! Finds the IMAP server for an address, the way the setup assistant does,
//! or signs in and reports what tab would read.
//! `cargo run -p tab-core --example email_find -- you@example.com`
//! `cargo run -p tab-core --example email_find -- --check host port user pass`

use tab_core::config::ConnectorConfig;
use tab_core::connectors::email;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    if a.first().map(String::as_str) == Some("--check") {
        let mut cfg = ConnectorConfig::on();
        for (k, v) in [
            ("host", &a[1]),
            ("port", &a[2]),
            ("username", &a[3]),
            ("password", &a[4]),
        ] {
            let v = if k == "port" {
                serde_json::json!(v.parse::<u64>()?)
            } else {
                v.clone().into()
            };
            cfg.settings.insert(k.into(), v);
        }
        match email::check(&cfg).await {
            Ok(r) => println!("{r}"),
            Err(e) => println!("failed: {e:#}"),
        }
        return Ok(());
    }
    let address = a
        .first()
        .cloned()
        .unwrap_or_else(|| "someone@gmail.com".into());
    let found = email::discover(&address).await?;
    println!("{}", serde_json::to_string_pretty(&found)?);
    Ok(())
}
