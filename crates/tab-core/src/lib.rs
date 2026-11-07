//! `tab-core` is the UI-agnostic heart of tab.
//!
//! Data flows in one direction:
//!
//! ```text
//!  Connector (GitHub, Slack, demo, ...)
//!      │  IncomingEvent
//!      ▼
//!  Engine ──► Store.upsert ──► Ranker (Haiku / heuristic) ──► Store.set_ranking
//!      │
//!      └──► broadcast EngineEvent (the desktop shell forwards these to the UI)
//! ```
//!
//! Nothing in this crate depends on Tauri, so it can be tested headless and
//! reused by the MCP server or a CLI later.
//!
//! The pure parts (item model, ranking rules, workflow definitions and
//! validation, link detection) live in `tab-domain`, which has no I/O and
//! builds for Wasm so the cloud hub can share them. They're re-exported here
//! under their old paths (`tab_core::model`, `tab_core::links`, ...), so code
//! that used them doesn't change. See `docs/ARCHITECTURE.md` for the crate map.

pub mod account;
pub mod agents;
pub mod cloud;
pub mod config;
pub mod connector;
pub mod connectors;
pub mod deps;
pub mod embed;
pub mod engine;
pub mod gateway;
pub mod gh;
pub mod hub_tls;
pub mod live;
pub mod local_hub;
pub mod lsp;
pub mod mcp;
pub mod ranker;
pub mod ratelimit;
pub mod sandbox;
pub mod storage;
pub mod store;
pub mod usage;
pub mod workflow;
pub mod workspace;

pub use tab_domain::{links, model};

pub use config::TabConfig;
pub use engine::{Change, Engine, EngineEvent, EngineHandle};
pub use gateway::Gateway;
pub use model::*;
pub use store::Store;
