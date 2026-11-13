use std::sync::Arc;

use tab_core::e2e::Files;
use tab_core::local_hub::LocalHub;

/// Sealed vault files go into the hub's file store, like any file.
pub struct HubFiles(pub Arc<LocalHub>);

#[async_trait::async_trait]
impl Files for HubFiles {
    async fn put(&self, bytes: Vec<u8>) -> anyhow::Result<String> {
        let hub = self.0.clone();
        Ok(tokio::task::spawn_blocking(move || hub.file_put(&bytes))
            .await??
            .hash)
    }

    async fn get(&self, hash: &str) -> anyhow::Result<Option<Vec<u8>>> {
        let (hub, hash) = (self.0.clone(), hash.to_string());
        tokio::task::spawn_blocking(move || hub.file_bytes(&hash)).await?
    }
}
