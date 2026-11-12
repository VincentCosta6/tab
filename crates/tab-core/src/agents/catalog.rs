//! What tab knows about each agent harness, as data (`harnesses.json`): its
//! name and command, its models (a fixed list, or a command that lists
//! them), the ones shown first, and how to install and update it. The
//! pickers and onboarding read this, so a new harness is a new entry plus
//! its adapter, not new UI.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

#[cfg(test)]
use super::default_command;
use super::{AgentHub, command};
use crate::model::AgentKind;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Harness {
    /// Matches an adapter (`AgentKind`). Entries without one are skipped.
    pub id: String,
    pub name: String,
    pub icon: Icon,
    /// The executable, looked up on PATH and in `bin_dirs`.
    pub command: String,
    pub pitch: String,
    pub capabilities: Capabilities,
    /// Help text for typing a model by hand.
    pub model_hint: String,
    /// The model background helpers (titler, suggestions) use when none is
    /// set. `None` means the harness's own default.
    pub small_model: Option<String>,
    /// Models it always offers.
    #[serde(default)]
    pub models: Vec<ModelSpec>,
    /// A command that lists more, for harnesses that know their providers.
    pub list_models: Option<ListModels>,
    /// Listed models shown first.
    #[serde(default)]
    pub common_models: Vec<String>,
    pub install: Install,
    pub upgrade: Upgrade,
    /// Where its installers put it, relative to home, for when the app's
    /// PATH doesn't include them yet.
    #[serde(default)]
    pub bin_dirs: Vec<String>,
    /// How to run it over the Agent Client Protocol (see `agents::acp`),
    /// for harnesses whose chats can go through it.
    #[serde(default)]
    pub acp: Option<AcpSpec>,
}

/// An ACP agent as data: the program, its arguments and environment, and
/// what strict mode adds. A new ACP agent is a new entry.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AcpSpec {
    /// The program; `None` runs the harness's own command (it speaks ACP).
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: std::collections::BTreeMap<String, String>,
    /// Added in strict mode, to keep the person's own setup out.
    #[serde(default)]
    pub strict_args: Vec<String>,
    #[serde(default)]
    pub strict_env: std::collections::BTreeMap<String, String>,
    /// How it names MCP tools (`mcp__{server}__{tool}` or
    /// `{server}_{tool}`), so strict mode can spot a server tab didn't add.
    #[serde(default)]
    pub mcp_tool_name: Option<String>,
    /// The adapter, when it's a program of its own. Its license is checked
    /// before tab offers it (see `every_acp_adapter_has_an_allowed_license`).
    #[serde(default)]
    pub adapter: Option<AcpAdapter>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AcpAdapter {
    pub npm: Option<String>,
    pub repo: String,
    /// SPDX id, as read from the repository.
    pub license: String,
}

impl AcpSpec {
    pub fn args_for(&self, strict: bool) -> Vec<std::ffi::OsString> {
        let extra = if strict { &self.strict_args[..] } else { &[] };
        self.args.iter().chain(extra).map(Into::into).collect()
    }

    pub fn env_for(&self, strict: bool) -> Vec<(&str, &str)> {
        let extra = strict.then_some(&self.strict_env).into_iter().flatten();
        self.env
            .iter()
            .chain(extra)
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect()
    }
}

/// How the app draws it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Icon {
    /// A brand mark the app ships (`claude`, `opencode`).
    pub brand: Option<String>,
    /// Or an SVG of its own, for harnesses the app has no mark for.
    #[serde(default)]
    pub svg: Option<String>,
}

/// What the harness can do, so the app shows only the controls that apply.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Capabilities {
    /// Its adapter can keep the person's own setup out and report what loaded.
    pub strict: bool,
    /// Permission modes it takes; empty hides the picker.
    #[serde(default)]
    pub permission_modes: Vec<Choice>,
    /// It can attach to a server the person runs (where strict can't apply).
    #[serde(default)]
    pub external_server: bool,
    /// It stops a turn past a spend cap (`per_turn_usd`).
    #[serde(default)]
    pub budget_cap: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Choice {
    pub value: String,
    pub label: String,
    #[serde(default)]
    pub help: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelSpec {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub common: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ListModels {
    pub args: Vec<String>,
    /// How to read its output. `provider_slash_model_lines`: one
    /// `provider/model` per line. `provider_model_table`: a table whose
    /// first two columns are the provider and the model.
    pub format: String,
}

/// How to install it: through npm when that's on PATH, else its own
/// installer script.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Install {
    pub npm: Option<String>,
    pub unix: Option<String>,
    pub windows: Option<String>,
}

/// The update command for each way it can be installed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Upgrade {
    pub winget: Option<String>,
    pub npm: Option<String>,
    pub brew: Option<String>,
    pub native: Option<String>,
}

impl Harness {
    pub fn kind(&self) -> Option<AgentKind> {
        AgentKind::ALL.into_iter().find(|k| k.as_str() == self.id)
    }

    /// The command that installs it on this computer, if tab can run one.
    pub fn install_command(&self) -> Option<String> {
        if let Some(pkg) = &self.install.npm
            && which::which("npm").is_ok()
        {
            return Some(format!("npm install -g {pkg}"));
        }
        if cfg!(windows) {
            self.install.windows.clone()
        } else {
            self.install.unix.clone()
        }
    }
}

static HARNESSES: LazyLock<Vec<Harness>> = LazyLock::new(|| {
    serde_json::from_str::<Vec<Harness>>(include_str!("harnesses.json"))
        .expect("harnesses.json is valid")
        .into_iter()
        .filter(|h| h.kind().is_some())
        .collect()
});

/// Every harness tab has an adapter for.
pub fn harnesses() -> &'static [Harness] {
    &HARNESSES
}

pub fn harness(kind: AgentKind) -> &'static Harness {
    HARNESSES
        .iter()
        .find(|h| h.kind() == Some(kind))
        .expect("every AgentKind has an entry in harnesses.json")
}

/// Where a harness's installers put it, for when PATH doesn't have it yet
/// (an app started before the install keeps its old PATH).
pub fn installed_elsewhere(kind: AgentKind) -> Option<PathBuf> {
    let home = crate::sandbox::home()?;
    let h = harness(kind);
    let names: Vec<String> = if cfg!(windows) {
        ["exe", "cmd"]
            .iter()
            .map(|e| format!("{}.{e}", h.command))
            .collect()
    } else {
        vec![h.command.clone()]
    };
    // Homebrew's folders too, which the app's PATH lacks when opened from
    // Finder.
    let system: &[&str] = if cfg!(windows) {
        &[]
    } else {
        &["/opt/homebrew/bin", "/usr/local/bin"]
    };
    h.bin_dirs
        .iter()
        .map(|d| home.join(d))
        .chain(system.iter().map(PathBuf::from))
        .flat_map(|d| names.iter().map(|n| d.join(n)).collect::<Vec<_>>())
        .find(|p| p.is_file())
}

/// One model a harness can run.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ModelOption {
    pub agent: AgentKind,
    /// What the harness is given: `claude-opus-5-5`, `anthropic/claude-sonnet-5-5`.
    pub id: String,
    pub label: String,
    /// Shown first, before the long tail.
    pub common: bool,
}

/// A harness's fixed models, then what its list command printed.
pub fn options(h: &Harness, listed: &str) -> Vec<ModelOption> {
    let Some(agent) = h.kind() else {
        return Vec::new();
    };
    let mut seen = std::collections::HashSet::new();
    let fixed = h.models.iter().map(|m| ModelOption {
        agent,
        id: m.id.clone(),
        label: m.label.clone(),
        common: m.common,
    });
    let format = h.list_models.as_ref().map(|l| l.format.as_str());
    let ids: Vec<String> = match format {
        Some("provider_slash_model_lines") => listed
            .lines()
            .map(str::trim)
            .filter(|l| !l.contains(char::is_whitespace))
            .map(str::to_string)
            .collect(),
        // A table whose first two columns are provider and model, under a
        // `provider model …` header.
        Some("provider_model_table") => {
            let cols = |l: &str| {
                let mut c = l.split_whitespace();
                Some((c.next()?.to_string(), c.next()?.to_string()))
            };
            listed
                .lines()
                .skip_while(|l| cols(l).is_none_or(|(p, m)| p != "provider" || m != "model"))
                .skip(1)
                .filter_map(cols)
                .map(|(p, m)| format!("{p}/{m}"))
                .collect()
        }
        _ => Vec::new(),
    };
    let mut more: Vec<ModelOption> = ids
        .iter()
        .filter(|id| {
            id.split_once('/')
                .is_some_and(|(p, m)| !p.is_empty() && !m.is_empty())
        })
        .map(|id| ModelOption {
            agent,
            id: id.clone(),
            label: id.split_once('/').map(|(_, m)| m).unwrap_or(id).into(),
            common: h.common_models.iter().any(|c| c == id),
        })
        .collect();
    more.sort_by(|a, b| b.common.cmp(&a.common).then_with(|| a.id.cmp(&b.id)));
    fixed
        .chain(more)
        .filter(|m| seen.insert(m.id.clone()))
        .collect()
}

async fn list_output(h: &Harness, exe: &Path) -> String {
    let Some(list) = &h.list_models else {
        return String::new();
    };
    let out = tokio::time::timeout(
        Duration::from_secs(20),
        command(exe).args(&list.args).output(),
    )
    .await;
    match out {
        Ok(Ok(o)) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
        _ => String::new(),
    }
}

impl AgentHub {
    /// Where `kind`'s executable is: the configured command for the harness
    /// in use, else PATH, else where its installer puts it.
    pub fn executable(&self, kind: AgentKind) -> Option<PathBuf> {
        super::agent_exe(&self.config(), kind).ok()
    }

    /// Every model of every installed harness. Listed ones are kept for ten
    /// minutes, since listing can ask each provider.
    pub async fn models(&self) -> Vec<ModelOption> {
        use std::collections::HashMap;
        use std::sync::Mutex;
        static CACHE: LazyLock<Mutex<HashMap<String, (Instant, String)>>> =
            LazyLock::new(Default::default);
        let mut out = Vec::new();
        for h in harnesses() {
            let Some(kind) = h.kind() else { continue };
            let Some(exe) = self.executable(kind) else {
                continue;
            };
            let cached = CACHE
                .lock()
                .unwrap()
                .get(&h.id)
                .filter(|(at, s)| at.elapsed() < Duration::from_secs(600) && !s.is_empty())
                .map(|(_, s)| s.clone());
            let listed = match cached {
                Some(s) => s,
                None => {
                    let s = list_output(h, &exe).await;
                    CACHE
                        .lock()
                        .unwrap()
                        .insert(h.id.clone(), (Instant::now(), s.clone()));
                    s
                }
            };
            out.extend(options(h, &listed));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_custom_command_that_is_the_default_name_is_found_like_no_custom() {
        for kind in [AgentKind::ClaudeCode, AgentKind::OpenCode] {
            let bare = default_command(kind);
            assert_eq!(
                super::super::find(kind, Some(bare)).ok(),
                super::super::find(kind, None).ok()
            );
            assert_eq!(
                super::super::find(kind, Some(&format!(" {bare} "))).ok(),
                super::super::find(kind, None).ok()
            );
        }
    }

    #[test]
    fn every_acp_adapter_has_an_allowed_license() {
        // Same list as deny.toml: tab only offers adapters it could ship.
        const ALLOWED: &[&str] = &["MIT", "Apache-2.0", "BSD-2-Clause", "BSD-3-Clause", "ISC"];
        for h in harnesses() {
            if let Some(a) = h.acp.as_ref().and_then(|a| a.adapter.as_ref()) {
                assert!(
                    ALLOWED.contains(&a.license.as_str()),
                    "{}: {}",
                    h.id,
                    a.license
                );
            }
        }
    }

    #[test]
    fn strict_adds_its_arguments_and_environment() {
        let h = harness(AgentKind::OpenCode);
        let acp = h.acp.as_ref().expect("OpenCode speaks ACP");
        assert_eq!(acp.args_for(false), ["acp"]);
        assert!(
            acp.env_for(true)
                .contains(&("OPENCODE_DISABLE_CLAUDE_CODE", "1"))
        );
        assert!(acp.env_for(false).is_empty());
    }

    #[test]
    fn every_adapter_has_metadata() {
        for kind in AgentKind::ALL {
            let h = harness(kind);
            assert_eq!(h.command, default_command(kind));
            assert!(h.install.npm.is_some() || h.install.unix.is_some());
            assert_eq!(h.kind(), Some(kind));
            // The id is how the app and the config spell the harness.
            assert_eq!(serde_json::to_value(kind).unwrap(), h.id.as_str());
        }
    }

    #[test]
    fn listed_models_follow_the_fixed_ones_common_first() {
        let oc = harness(AgentKind::OpenCode);
        let out = "Some notice\n\nopenai/gpt-5\nanthropic/claude-sonnet-5-5\nanthropic/claude-sonnet-5-5\nnot a model\nollama/qwen3:32b\n";
        let m = options(oc, out);
        let ids: Vec<_> = m.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "anthropic/claude-sonnet-5-5",
                "ollama/qwen3:32b",
                "openai/gpt-5"
            ]
        );
        assert!(m[0].common && !m[1].common);
        assert_eq!(m[0].label, "claude-sonnet-5-5");
    }

    #[test]
    fn a_model_table_reads_its_first_two_columns() {
        let pi = harness(AgentKind::Pi);
        let out = "Warning: errors loading models.json\nprovider   model              context  max-out  thinking  images\nanthropic  claude-sonnet-5-5  1M       64K      yes       yes\nopenai     gpt-6-sol          400K     128K     yes       yes\n";
        let ids: Vec<_> = options(pi, out)
            .into_iter()
            .map(|m| (m.id, m.common))
            .collect();
        assert_eq!(
            ids,
            [
                ("anthropic/claude-sonnet-5-5".to_string(), true),
                ("openai/gpt-6-sol".to_string(), true),
            ]
        );
    }

    #[test]
    fn a_fixed_list_needs_no_command() {
        let m = options(harness(AgentKind::ClaudeCode), "");
        assert!(m.iter().take_while(|m| m.common).count() >= 3);
        assert!(m.iter().all(|m| m.agent == AgentKind::ClaudeCode));
    }
}
