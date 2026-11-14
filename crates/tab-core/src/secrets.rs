//! Connection secrets (passwords, tokens, the ranker's API key) live in the
//! [`Vault`], not in `config.json`.
//!
//! In memory, [`TabConfig`] still carries them where they always were (a
//! connection's settings, `ranking.ai.api_key`), so nothing that reads the
//! config changes. [`TabConfig::save`] lifts them out into one vault entry
//! and writes the rest; [`TabConfig::load`] puts them back.
//!
//! Older versions wrote them into `config.json`. A secret found there is
//! taken as the newest (it was typed in last, or the vault couldn't be
//! written), and the next save moves it into the vault.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::TabConfig;
use crate::model::SettingKind;
use crate::vault::Vault;

/// The vault entry holding every connection secret.
const ENTRY: &str = "config";

/// The folder beside `config.json` the vault keeps its files in (Windows,
/// Linux). Hidden from agents ([`crate::sandbox`]).
pub const DIR: &str = "connection-secrets";

/// The vault for the config at `config_path`.
pub fn vault_for(config_path: &Path) -> Vault {
    let dir = config_path
        .parent()
        .map(|d| d.join(DIR))
        .unwrap_or_else(|| PathBuf::from(DIR));
    Vault::named(dir, "tab connection secrets")
}

static INSTALLED: OnceLock<PathBuf> = OnceLock::new();

/// Remembers where the app's config is, so code without it at hand (an
/// email connection refreshing its sign-in) can reach the same vault.
pub fn install(config_path: &Path) {
    let _ = INSTALLED.set(config_path.to_path_buf());
}

/// The app's vault, once [`install`]ed.
pub fn vault() -> Option<Vault> {
    INSTALLED.get().map(|p| vault_for(p))
}

static IN_FILE: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());

/// The vault couldn't be read when `config_path` was loaded: until the next
/// launch, saves keep secrets in the file so what the vault holds isn't
/// overwritten with nothing.
pub(crate) fn keep_in_file(config_path: &Path) {
    IN_FILE.lock().unwrap().push(config_path.to_path_buf());
}

pub(crate) fn kept_in_file(config_path: &Path) -> bool {
    IN_FILE.lock().unwrap().iter().any(|p| p == config_path)
}

/// `(connector id, setting key)` of every setting that holds a secret,
/// from the connectors' descriptors.
pub fn secret_keys() -> Vec<(String, String)> {
    crate::connectors::registry()
        .into_iter()
        .flat_map(|f| {
            let d = f.descriptor();
            d.settings
                .into_iter()
                .filter(|s| s.kind == SettingKind::Secret)
                .map(move |s| (d.id.clone(), s.key))
        })
        .collect()
}

/// What's kept in the vault entry.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub(crate) struct Stored {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai_api_key: Option<String>,
    /// Connector id → setting key → value.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub connectors: BTreeMap<String, serde_json::Map<String, serde_json::Value>>,
}

impl Stored {
    pub fn is_empty(&self) -> bool {
        self.ai_api_key.is_none() && self.connectors.is_empty()
    }
}

fn present(v: &serde_json::Value) -> bool {
    !v.is_null() && v.as_str().is_none_or(|s| !s.is_empty())
}

/// Splits `cfg` into what goes in `config.json` and what goes in the vault.
pub(crate) fn split(cfg: &TabConfig) -> (TabConfig, Stored) {
    let mut file = cfg.clone();
    let mut stored = Stored::default();
    if let Some(k) = file.ranking.ai.api_key.take().filter(|k| !k.is_empty()) {
        stored.ai_api_key = Some(k);
    }
    for (id, key) in secret_keys() {
        let Some(c) = file.connectors.get_mut(&id) else {
            continue;
        };
        if let Some(v) = c.settings.remove(&key).filter(present) {
            stored.connectors.entry(id).or_default().insert(key, v);
        }
    }
    (file, stored)
}

/// Whether `config.json` as read still holds secrets (written by an older
/// version, or when the vault couldn't be written).
pub(crate) fn has_plaintext(cfg: &TabConfig) -> bool {
    !split(cfg).1.is_empty()
}

/// Puts the vault's secrets back into `cfg`. One already in `cfg` (from
/// `config.json`) is newer and stays.
pub(crate) fn merge(cfg: &mut TabConfig, stored: Stored) {
    if cfg.ranking.ai.api_key.as_deref().is_none_or(str::is_empty) {
        cfg.ranking.ai.api_key = stored.ai_api_key;
    }
    for (id, values) in stored.connectors {
        // A connection removed from the file is gone; its secrets go too.
        let Some(c) = cfg.connectors.get_mut(&id) else {
            continue;
        };
        for (k, v) in values {
            if !c.settings.get(&k).is_some_and(present) {
                c.settings.insert(k, v);
            }
        }
    }
}

/// Reads the vault entry. `Err` when it's there but can't be read.
pub(crate) fn read(v: &Vault) -> anyhow::Result<Stored> {
    match v.try_get(ENTRY)? {
        Some(s) => Ok(serde_json::from_str(&s)?),
        None => Ok(Stored::default()),
    }
}

/// Writes the vault entry (or removes it when there's nothing to keep).
/// Skips the write when nothing changed.
pub(crate) fn write(v: &Vault, stored: &Stored) -> anyhow::Result<()> {
    if stored.is_empty() {
        return v.try_remove(ENTRY);
    }
    let json = serde_json::to_string(stored)?;
    if v.try_get(ENTRY).ok().flatten().as_deref() == Some(json.as_str()) {
        return Ok(());
    }
    v.set(ENTRY, &json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ConnectorConfig;

    fn with_secrets() -> TabConfig {
        let mut cfg = TabConfig::default();
        cfg.ranking.ai.api_key = Some("sk-ant".into());
        let mut email = ConnectorConfig::on();
        email
            .settings
            .insert("host".into(), "imap.example.com".into());
        email.settings.insert("password".into(), "hunter2".into());
        cfg.connectors.insert("email".into(), email);
        let mut gh = ConnectorConfig::on();
        gh.settings.insert("token".into(), "ghp_x".into());
        cfg.connectors.insert("github".into(), gh);
        cfg
    }

    #[test]
    fn split_lifts_every_secret_and_merge_puts_them_back() {
        let cfg = with_secrets();
        let (file, stored) = split(&cfg);
        let text = serde_json::to_string(&file).unwrap();
        for s in ["sk-ant", "hunter2", "ghp_x"] {
            assert!(!text.contains(s), "{s} stayed in config.json");
        }
        assert!(text.contains("imap.example.com"));
        assert_eq!(stored.ai_api_key.as_deref(), Some("sk-ant"));
        assert_eq!(stored.connectors["email"]["password"], "hunter2");
        assert!(!has_plaintext(&file));
        assert!(has_plaintext(&cfg));

        let mut back = file.clone();
        merge(&mut back, stored);
        assert_eq!(back, cfg);
    }

    #[test]
    fn a_secret_in_the_file_is_newer_than_the_vaults() {
        let mut cfg = with_secrets();
        let (_, stored) = split(&cfg);
        cfg.connectors
            .get_mut("email")
            .unwrap()
            .settings
            .insert("password".into(), "new".into());
        merge(&mut cfg, stored);
        assert_eq!(cfg.connectors["email"].str("password"), Some("new"));
    }

    #[test]
    fn a_removed_connection_doesnt_come_back_from_the_vault() {
        let cfg = with_secrets();
        let (mut file, stored) = split(&cfg);
        file.connectors.remove("email");
        merge(&mut file, stored);
        assert!(!file.connectors.contains_key("email"));
        assert_eq!(file.connectors["github"].str("token"), Some("ghp_x"));
    }

    #[test]
    fn empty_secrets_arent_kept() {
        let mut cfg = TabConfig::default();
        cfg.ranking.ai.api_key = Some(String::new());
        let mut email = ConnectorConfig::on();
        email.settings.insert("password".into(), "".into());
        cfg.connectors.insert("email".into(), email);
        assert!(split(&cfg).1.is_empty());
    }
}
