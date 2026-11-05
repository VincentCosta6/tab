use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::model::UserProfile;
use crate::ranker::RankingRules;

/// User configuration, persisted as JSON in the app config dir.
///
/// Secrets live here for now; moving them to the OS keychain is planned.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TabConfig {
    #[serde(default)]
    pub profile: UserProfile,
    #[serde(default)]
    pub ranking: RankingConfig,
    /// Keyed by connector id. Settings are free-form so new connectors need
    /// no config changes; each connector declares its fields in its descriptor.
    #[serde(default = "default_connectors")]
    pub connectors: BTreeMap<String, ConnectorConfig>,
}

impl Default for TabConfig {
    fn default() -> Self {
        Self {
            profile: UserProfile::default(),
            ranking: RankingConfig::default(),
            connectors: default_connectors(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RankingConfig {
    #[serde(default)]
    pub rules: RankingRules,
    #[serde(default)]
    pub ai: AiConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AiConfig {
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    /// The person's own words about what matters, e.g. "Anything from my
    /// manager is urgent. Dependabot PRs can wait until Friday."
    #[serde(default)]
    pub instructions: String,
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            api_key: None,
            model: None,
            instructions: String::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ConnectorConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub settings: serde_json::Map<String, serde_json::Value>,
}

impl ConnectorConfig {
    pub fn str(&self, key: &str) -> Option<&str> {
        self.settings.get(key)?.as_str().filter(|s| !s.is_empty())
    }
    pub fn u64(&self, key: &str) -> Option<u64> {
        let v = self.settings.get(key)?;
        v.as_u64().or_else(|| v.as_str()?.parse().ok())
    }
}

fn yes() -> bool {
    true
}

fn default_connectors() -> BTreeMap<String, ConnectorConfig> {
    // Sample data is on for a fresh install so the queue isn't empty.
    BTreeMap::from([(
        "demo".to_string(),
        ConnectorConfig {
            enabled: true,
            settings: Default::default(),
        },
    )])
}

impl TabConfig {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let mut cfg = match std::fs::read_to_string(path) {
            Ok(s) => serde_json::from_str(&s)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => TabConfig::default(),
            Err(e) => return Err(e.into()),
        };
        cfg.apply_env();
        Ok(cfg)
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    pub fn connector(&self, id: &str) -> Option<&ConnectorConfig> {
        self.connectors.get(id).filter(|c| c.enabled)
    }

    /// Environment variables fill in anything the config file leaves empty,
    /// which keeps `pnpm tauri dev` easy.
    fn apply_env(&mut self) {
        if self.ranking.ai.api_key.as_deref().is_none_or(str::is_empty) {
            self.ranking.ai.api_key = std::env::var("ANTHROPIC_API_KEY")
                .ok()
                .filter(|s| !s.is_empty());
        }
        if !self.connectors.contains_key("github")
            && let Ok(token) = std::env::var("GITHUB_TOKEN").map(|t| t.trim().to_string())
            && !token.is_empty()
        {
            let mut settings = serde_json::Map::new();
            settings.insert("token".into(), token.into());
            self.connectors.insert(
                "github".into(),
                ConnectorConfig {
                    enabled: true,
                    settings,
                },
            );
        }
    }
}
