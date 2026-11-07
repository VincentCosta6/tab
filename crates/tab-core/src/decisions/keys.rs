//! Where a provider's key comes from, in order: the key in settings, the
//! provider's environment variable, then a harness that already has one,
//! but only when the person chose to reuse it (`key_from`).

use std::path::PathBuf;

use serde_json::Value;

use super::Provider;
use crate::config::DecisionConfig;

pub(super) fn resolve(cfg: &DecisionConfig, p: &Provider) -> Option<String> {
    let own = cfg
        .api_keys
        .get(&p.id)
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty());
    own.or_else(|| {
        std::env::var(&p.env)
            .ok()
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty())
    })
    .or_else(|| {
        let harness = cfg.key_from.get(&p.id)?;
        p.key_from
            .iter()
            .any(|h| h == harness)
            .then(|| harness_key(harness, &p.id))
            .flatten()
    })
}

/// The API key a harness has saved for a provider, if any. Only keys,
/// never sign-ins: a ChatGPT or Claude login doesn't reach decision APIs.
pub fn harness_key(harness: &str, provider: &str) -> Option<String> {
    match harness {
        "opencode" => opencode_auth_files()
            .into_iter()
            .filter_map(|f| std::fs::read_to_string(f).ok())
            .find_map(|text| opencode_key(&text, provider)),
        _ => None,
    }
}

/// OpenCode keeps the keys from `opencode auth login` in `auth.json` under
/// its data folder (`$XDG_DATA_HOME/opencode`, else `~/.local/share/opencode`).
fn opencode_auth_files() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(d) = std::env::var_os("XDG_DATA_HOME").filter(|d| !d.is_empty()) {
        out.push(PathBuf::from(d).join("opencode/auth.json"));
    }
    if let Some(h) = crate::sandbox::home() {
        out.push(h.join(".local/share/opencode/auth.json"));
    }
    out
}

/// `{"openrouter": {"type": "api", "key": "sk-or-..."}}`; OAuth entries
/// (`"type": "oauth"`) are subscriptions and are skipped.
pub(super) fn opencode_key(auth_json: &str, provider: &str) -> Option<String> {
    let v: Value = serde_json::from_str(auth_json).ok()?;
    let e = &v[provider];
    (e["type"] == "api")
        .then(|| e["key"].as_str())
        .flatten()
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .map(str::to_string)
}
