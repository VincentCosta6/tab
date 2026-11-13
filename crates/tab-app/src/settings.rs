//! Reading and saving the config for Settings. Secrets go out masked and
//! come back unchanged unless the person typed a new one.

use tab_core::TabConfig;
use tab_core::secrets::secret_keys;

use crate::Backend;

/// What Settings shows in place of a stored secret.
pub const SECRET_MASK: &str = "••••••••";

/// `cfg` with its secrets replaced by [`SECRET_MASK`].
pub fn masked(cfg: &TabConfig) -> TabConfig {
    let mut cfg = cfg.clone();
    if cfg.ranking.ai.api_key.is_some() {
        cfg.ranking.ai.api_key = Some(SECRET_MASK.into());
    }
    for (id, key) in secret_keys() {
        if let Some(v) = cfg
            .connectors
            .get_mut(&id)
            .and_then(|c| c.settings.get_mut(&key))
            && v.as_str().is_some_and(|s| !s.is_empty())
        {
            *v = SECRET_MASK.into();
        }
    }
    cfg
}

/// Puts `old`'s secrets back wherever `new` still has the mask.
pub fn unmask(mut new: TabConfig, old: &TabConfig) -> TabConfig {
    if new.ranking.ai.api_key.as_deref() == Some(SECRET_MASK) {
        new.ranking.ai.api_key = old.ranking.ai.api_key.clone();
    }
    for (id, key) in secret_keys() {
        let keep = new
            .connectors
            .get(&id)
            .and_then(|c| c.settings.get(&key))
            .and_then(|v| v.as_str())
            == Some(SECRET_MASK);
        if keep {
            let prev = old
                .connectors
                .get(&id)
                .and_then(|c| c.settings.get(&key))
                .cloned()
                .unwrap_or_default();
            if let Some(c) = new.connectors.get_mut(&id) {
                c.settings.insert(key, prev);
            }
        }
    }
    new
}

impl Backend {
    /// The config as Settings shows it.
    pub async fn config_masked(&self) -> TabConfig {
        masked(&*self.config.lock().await)
    }

    /// Saves what Settings sends back, keeping secrets it didn't change.
    pub async fn config_save(&self, config: TabConfig) -> Result<TabConfig, String> {
        let current = self.config.lock().await;
        let mut next = unmask(config, &current);
        // Only moving sets the cloud hub: Settings can't undo it.
        next.hub.cloud_hub = current.hub.cloud_hub.clone();
        if next.hub.cloud_hub.is_some() {
            next.hub.mode = tab_core::config::HubMode::Cloud;
        }
        drop(current);
        let saved = self.apply(next).await.map_err(crate::err)?;
        Ok(masked(&saved))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_masked_key_comes_back_unchanged() {
        let mut old = TabConfig::default();
        old.ranking.ai.api_key = Some("sk-real".into());
        let shown = masked(&old);
        assert_eq!(shown.ranking.ai.api_key.as_deref(), Some(SECRET_MASK));
        assert_eq!(
            unmask(shown, &old).ranking.ai.api_key.as_deref(),
            Some("sk-real")
        );
        let mut typed = masked(&old);
        typed.ranking.ai.api_key = Some("sk-new".into());
        assert_eq!(
            unmask(typed, &old).ranking.ai.api_key.as_deref(),
            Some("sk-new")
        );
    }
}
