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

pub mod agents;
pub mod config;
pub mod connector;
pub mod connectors;
pub mod embed;
pub mod engine;
pub mod gateway;
pub mod gh;
pub mod links;
pub mod lsp;
pub mod mcp;
pub mod model;
pub mod ranker;
pub mod ratelimit;
pub mod store;
pub mod usage;
pub mod workflow;
pub mod workspace;

pub use config::TabConfig;
pub use engine::{Change, Engine, EngineEvent, EngineHandle};
pub use gateway::Gateway;
pub use model::*;
pub use store::Store;
