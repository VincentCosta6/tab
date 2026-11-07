//! `tab-domain` is tab's pure logic: the shapes of work and the rules about
//! them, with no I/O.
//!
//! - [`model`]: queue items, connectors' descriptions of themselves, context,
//!   sessions.
//! - [`rules`]: deterministic ranking rules (the floor the queue relies on).
//! - [`lease`]: who is working on what, and for how long.
//! - [`ops`]: the changes a hub applies and replicates.
//! - [`blob`]: files, named by the hash of their bytes, that records point to.
//! - [`workflow`]: workflow definitions, validation and templates.
//! - [`decision`]: questions for decision models and their typed answers.
//! - [`links`]: recognizing links agents create or mention.
//!
//! Every place a hub runs uses this crate: the desktop engine (`tab-core`),
//! the hub library (`tab-hub`), and later the Cloudflare Worker. So nothing
//! here may touch the network, the filesystem, threads or the clock. That's
//! what lets it build for `wasm32-unknown-unknown`, and CI keeps it that way.

pub mod blob;
pub mod cli;
pub mod decision;
pub mod lease;
pub mod links;
pub mod model;
pub mod ops;
pub mod rules;
pub mod workflow;

pub use cli::*;
pub use model::*;
