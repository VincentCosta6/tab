//! Built-in connectors. Each file is one integration; [`registry`] lists them all.

mod demo;
mod github;
pub mod github_review;
mod planned;
pub mod tab;

use std::sync::Arc;

pub use demo::{DemoConnector, DemoFactory};
pub use github::{GithubConnector, GithubFactory};
pub use tab::{TabConnector, TabFactory};

use crate::TabConfig;
use crate::connector::{Connector, ConnectorFactory};
use crate::gateway::Gateway;

/// Every connector type the app knows about, in display order.
pub fn registry() -> Vec<Box<dyn ConnectorFactory>> {
    let mut v: Vec<Box<dyn ConnectorFactory>> =
        vec![Box::new(TabFactory), Box::new(GithubFactory), Box::new(DemoFactory)];
    v.extend(planned::factories());
    v
}

/// Connectors that failed to build, as `(connector id, message)`.
pub type BuildFailures = Vec<(String, String)>;

/// Builds the enabled connectors. Connectors that fail to build are returned
/// as errors so the engine can show them in the Connections tab.
pub fn build_enabled(
    cfg: &TabConfig,
    gateway: Arc<Gateway>,
) -> (Vec<Arc<dyn Connector>>, BuildFailures) {
    let mut ok = Vec::new();
    let mut failed = Vec::new();
    for f in registry() {
        let id = f.descriptor().id;
        let Some(c) = cfg.connector(&id) else {
            continue;
        };
        match f.build(c, gateway.clone()) {
            Some(Ok(conn)) => ok.push(conn),
            Some(Err(e)) => failed.push((id, e.to_string())),
            None => {}
        }
    }
    (ok, failed)
}
