//! Built-in connectors. Each file is one integration; [`registry`] lists them all.

#[cfg(test)]
mod demo;
pub mod email;
mod github;
pub mod github_review;
pub mod tab;

use std::sync::Arc;

#[cfg(test)]
pub use demo::{DemoConnector, DemoFactory};
pub use email::{EmailConnector, EmailFactory};
pub use github::{GithubConnector, GithubFactory};
pub use tab::{TabConnector, TabFactory};

use crate::TabConfig;
use crate::connector::{Connector, ConnectorFactory};
use crate::gateway::Gateway;

/// Every connector type the app knows about, in display order.
pub fn registry() -> Vec<Box<dyn ConnectorFactory>> {
    #[allow(unused_mut)]
    let mut v: Vec<Box<dyn ConnectorFactory>> = vec![
        Box::new(TabFactory),
        Box::new(GithubFactory),
        Box::new(EmailFactory),
    ];
    // Sample data only feeds the engine's tests.
    #[cfg(test)]
    v.push(Box::new(DemoFactory));
    v
}

/// Has connector `id` drop what it cached about `pages`, after something
/// changed them. See [`ConnectorFactory::forget_pages`].
pub fn forget_pages(id: &str, pages: &[String]) {
    if pages.is_empty() {
        return;
    }
    if let Some(f) = registry().iter().find(|f| f.descriptor().id == id) {
        f.forget_pages(pages);
    }
}

/// Whether tab still has a connector with this id. Items from one it
/// dropped (like the old sample data) are cleared on start.
pub fn known(id: &str) -> bool {
    registry().iter().any(|f| f.descriptor().id == id)
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
        // Set to run in tab cloud: the cloud runner polls it, not this app.
        if !c.runs_here() {
            continue;
        }
        match f.build(c, gateway.clone()) {
            Some(Ok(conn)) => ok.push(conn),
            Some(Err(e)) => failed.push((id, e.to_string())),
            None => {}
        }
    }
    (ok, failed)
}
