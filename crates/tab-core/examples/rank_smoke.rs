//! Ranks one item with the AI layer as configured by default (no API key, so
//! your Claude plan through Claude Code), then prints spend and plan windows.
//! `cargo run -p tab-core --example rank_smoke`

use std::sync::Arc;

use chrono::Utc;
use tab_core::model::{EntityRef, IncomingEvent};
use tab_core::ranker::{Pipeline, Ranker};
use tab_core::store::Upsert;
use tab_core::{Gateway, Store, TabConfig};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let store = Arc::new(Store::in_memory()?);
    let gateway = Arc::new(Gateway::new(store.clone()));
    let mut cfg = TabConfig::default();
    cfg.ranking.ai.api_key = None;
    let pipeline = Pipeline::from_config(&cfg, gateway, store.clone());
    println!(
        "ai layer: {:?}",
        pipeline
            .ai
            .as_ref()
            .map(|a| (a.backend(), a.model().to_string()))
    );
    let e = IncomingEvent {
        source: "slack".into(),
        external_id: "1".into(),
        kind: "slack.mention".into(),
        title: "@you prod checkout is down, are you around?".into(),
        actor: Some("Priya (on-call)".into()),
        occurred_at: Utc::now(),
        entities: vec![EntityRef::new(
            "channel",
            "slack:incident",
            "#incident",
            "in",
        )],
        ..Default::default()
    };
    let Upsert::Inserted(item) = store.upsert(&e, Utc::now())? else {
        unreachable!()
    };
    let r = pipeline.rank(&item).await?;
    println!(
        "ranked: {} by {} ({:?}): {}",
        r.score, r.ranked_by, r.ai_adjustment, r.reason
    );
    println!("{:#?}", store.usage_summary(Utc::now())?);
    Ok(())
}
