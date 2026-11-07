use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::model::{AgentKind, UserProfile};
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
    #[serde(default)]
    pub agent: AgentConfig,
    #[serde(default)]
    pub mcp: McpConfig,
    #[serde(default)]
    pub storage: StorageConfig,
    /// What agents, language servers and workflow commands may touch.
    #[serde(default)]
    pub sandbox: crate::sandbox::SandboxConfig,
    /// Set once the first-launch onboarding is finished.
    #[serde(default)]
    pub onboarded: bool,
}

/// Which coding agent tab drives, and how.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentConfig {
    /// `None` until onboarding picks one.
    #[serde(default)]
    pub kind: Option<AgentKind>,
    /// Override for the executable (`claude`, `opencode`, or a full path).
    #[serde(default)]
    pub command: Option<String>,
    /// e.g. `sonnet` for Claude Code, `anthropic/claude-sonnet-5-5` for OpenCode.
    #[serde(default)]
    pub model: Option<String>,
    /// Folder sessions start in. Defaults to the home folder.
    #[serde(default)]
    pub workdir: Option<String>,
    /// Claude Code permission mode. Tool approvals that need a person are
    /// routed to tab's chat either way.
    #[serde(default = "default_permission_mode")]
    pub permission_mode: String,
    /// Use an OpenCode server that is already running instead of starting one.
    #[serde(default)]
    pub opencode_url: Option<String>,
    #[serde(default)]
    pub budget: BudgetConfig,
    /// Offer a likely next reply after each turn (Tab fills it, Enter sends).
    #[serde(default = "default_true")]
    pub suggest_replies: bool,
    /// Names chats started in tab with a small model as the work comes into focus.
    #[serde(default)]
    pub titler: TitlerConfig,
}

/// The titler: after the first few turns of a chat, a small model renames
/// its task so the queue says what the thread is actually doing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TitlerConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Claude Code model alias or id. Empty means `claude-haiku-5-5`.
    /// OpenCode uses its own configured model.
    #[serde(default)]
    pub model: Option<String>,
    /// The person's own rules, added after the default ones, e.g.
    /// "Start with the repo name" or "Use ticket ids when there is one".
    #[serde(default)]
    pub instructions: String,
}

impl Default for TitlerConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            model: None,
            instructions: String::new(),
        }
    }
}

/// Spend limits for agents and the ranker. Costs are what the agent reports:
/// real spend on an API key, the API-equivalent figure on a Claude plan.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BudgetConfig {
    /// New agent turns are refused once today's spend reaches this.
    #[serde(default)]
    pub daily_usd: Option<f64>,
    #[serde(default)]
    pub monthly_usd: Option<f64>,
    /// Claude Code stops a single turn past this (`--max-budget-usd`).
    #[serde(default)]
    pub per_turn_usd: Option<f64>,
    /// On a Claude plan, background queue reading pauses once any usage
    /// window passes this fraction, keeping the rest for your own work.
    #[serde(default = "default_ranker_plan_cap")]
    pub ranker_plan_cap: f64,
}

impl Default for BudgetConfig {
    fn default() -> Self {
        Self {
            daily_usd: None,
            monthly_usd: None,
            per_turn_usd: None,
            ranker_plan_cap: default_ranker_plan_cap(),
        }
    }
}

fn default_ranker_plan_cap() -> f64 {
    0.8
}

impl BudgetConfig {
    /// Why a new turn can't start, if a limit is reached.
    pub fn blocked(&self, s: &crate::store::UsageSummary) -> Option<String> {
        if let Some(d) = self.daily_usd.filter(|d| *d > 0.0 && s.today_usd >= *d) {
            return Some(format!(
                "Today's agent budget of ${d:.2} is used up (${:.2} spent). Raise it in Settings → Agent.",
                s.today_usd
            ));
        }
        if let Some(m) = self.monthly_usd.filter(|m| *m > 0.0 && s.month_usd >= *m) {
            return Some(format!(
                "This month's agent budget of ${m:.2} is used up (${:.2} spent). Raise it in Settings → Agent.",
                s.month_usd
            ));
        }
        None
    }
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            kind: None,
            command: None,
            model: None,
            workdir: None,
            permission_mode: default_permission_mode(),
            opencode_url: None,
            budget: BudgetConfig::default(),
            suggest_replies: true,
            titler: TitlerConfig::default(),
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_permission_mode() -> String {
    "auto".into()
}

/// tab's local MCP server, which agents use to search the queue and add context.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpConfig {
    #[serde(default = "default_mcp_port")]
    pub port: u16,
    /// Bearer token agents must send. Generated on first launch.
    #[serde(default)]
    pub token: String,
}

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            port: default_mcp_port(),
            token: String::new(),
        }
    }
}

fn default_mcp_port() -> u16 {
    47821
}

/// How long tab keeps chat attachments and review checkouts, and how much
/// space they may take.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StorageConfig {
    /// Attachments of chats idle this long, and review checkouts unopened
    /// this long, are removed.
    #[serde(default = "default_keep_days")]
    pub keep_days: u32,
    /// Past this many GB, the least recently used attachments and checkouts
    /// are removed until it fits. `None` for no limit.
    #[serde(default = "default_max_gb")]
    pub max_gb: Option<f64>,
}

fn default_keep_days() -> u32 {
    7
}

fn default_max_gb() -> Option<f64> {
    Some(5.0)
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            keep_days: default_keep_days(),
            max_gb: default_max_gb(),
        }
    }
}

impl StorageConfig {
    /// How long unused things are kept; at least a day.
    pub fn keep(&self) -> std::time::Duration {
        std::time::Duration::from_secs(u64::from(self.keep_days.max(1)) * 86_400)
    }

    /// The size limit in bytes, if any.
    pub fn max_bytes(&self) -> Option<u64> {
        self.max_gb
            .filter(|g| g.is_finite() && *g > 0.0)
            .map(|g| (g * 1024.0 * 1024.0 * 1024.0) as u64)
    }
}

impl Default for TabConfig {
    fn default() -> Self {
        Self {
            profile: UserProfile::default(),
            ranking: RankingConfig::default(),
            connectors: default_connectors(),
            agent: AgentConfig::default(),
            mcp: McpConfig::default(),
            storage: StorageConfig::default(),
            sandbox: Default::default(),
            onboarded: false,
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
    /// Where the ranker's Haiku calls go. `auto` uses the API key when there
    /// is one, otherwise your Claude plan through the Claude Code CLI.
    #[serde(default)]
    pub backend: AiBackend,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    /// The person's own words about what matters, e.g. "Anything from my
    /// manager is urgent. Dependabot PRs can wait until Friday."
    #[serde(default)]
    pub instructions: String,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AiBackend {
    #[default]
    Auto,
    Api,
    ClaudeCode,
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            backend: AiBackend::Auto,
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
    pub fn on() -> Self {
        Self {
            enabled: true,
            settings: Default::default(),
        }
    }
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
    // tab's own task connector is always on.
    // Sample data is opt-in from onboarding, never on behind its back.
    BTreeMap::from([("tab".to_string(), ConnectorConfig::on())])
}

impl TabConfig {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let mut cfg = match std::fs::read_to_string(path) {
            Ok(s) => serde_json::from_str(&s)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => TabConfig::default(),
            Err(e) => return Err(e.into()),
        };
        cfg.apply_env();
        // Always enabled, but keep its saved settings (search by meaning, etc.).
        cfg.connectors
            .entry("tab".into())
            .or_insert_with(ConnectorConfig::on)
            .enabled = true;
        if cfg.mcp.token.is_empty() {
            cfg.mcp.token = uuid::Uuid::new_v4().simple().to_string();
            // Persist it so agent configs written earlier stay valid.
            let _ = cfg.save(path);
        }
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

#[cfg(test)]
mod tests {
    #[test]
    fn storage_limit_defaults_to_5_gb_but_can_be_off() {
        let old: StorageConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(old.keep_days, 7);
        assert_eq!(old.max_bytes(), Some(5 * 1024 * 1024 * 1024));
        let off: StorageConfig = serde_json::from_str(r#"{"max_gb": null}"#).unwrap();
        assert_eq!(off.max_bytes(), None);
        let cfg: TabConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(cfg.storage, StorageConfig::default());
    }

    use super::*;

    #[test]
    fn load_keeps_saved_tab_settings() {
        let path = std::env::temp_dir().join(format!("tab-config-{}.json", uuid::Uuid::new_v4()));
        let mut cfg = TabConfig::default();
        let mut tab = ConnectorConfig::on();
        tab.settings.insert("semantic_search".into(), true.into());
        cfg.connectors.insert("tab".into(), tab);
        cfg.save(&path).unwrap();

        let loaded = TabConfig::load(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        let tab = loaded.connectors.get("tab").unwrap();
        assert!(tab.enabled);
        assert_eq!(tab.settings.get("semantic_search"), Some(&true.into()));
    }
}
