//! tab, headless: a hub's cloud runner.
//!
//! tab cloud starts one of these next to a person's hub (on paid plans), in
//! a container of its own. It is the same engine the desktop app runs,
//! without a window:
//!
//! - It syncs with the hub like any computer does (`tab.db` here is just
//!   another copy), as a runner the hub let in with a token of its own.
//! - It polls the connections the person set to run in tab cloud, with the
//!   sign-ins the hub keeps for it, so the queue fills while every computer
//!   is off.
//! - It runs workflows, sharing each trigger with the person's computers
//!   through the hub's leases, so a run happens once wherever it lands.
//! - It takes agent jobs whose placement allows the cloud, with the
//!   person's own harness setup and keys, cloning the repo it's asked to
//!   work on.
//!
//! Configured by environment (set by the hub when it starts the container):
//! `TAB_CLOUD_URL`, `TAB_HUB_ID`, `TAB_RUNNER_TOKEN`, and `TAB_DATA` (a
//! writable folder, `/data` by default).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tab_core::agents::AgentHub;
use tab_core::cloud::{self, CloudConfig};
use tab_core::cloud_link::{self, CloudLink, TokenSource};
use tab_core::config::{AgentConfig, ConnectorConfig, TabConfig};
use tab_core::engine::{Engine, EngineEvent, EngineHandle};
use tab_core::gateway::Gateway;
use tab_core::jobs::{CancellationToken, JobRunner};
use tab_core::local_hub::LocalHub;
use tab_core::runner::RunnerKind;
use tab_core::store::Store;
use tokio::sync::broadcast;

/// How often it re-reads which connections run here and their secrets.
const REFRESH: Duration = Duration::from_secs(60);

struct Env {
    api: String,
    hub: String,
    token: String,
    data: PathBuf,
}

impl Env {
    fn from_env() -> anyhow::Result<Self> {
        let need = |k: &str| {
            std::env::var(k)
                .ok()
                .filter(|v| !v.trim().is_empty())
                .ok_or_else(|| anyhow::anyhow!("{k} isn't set"))
        };
        Ok(Self {
            api: CloudConfig::from_env().api,
            hub: need("TAB_HUB_ID")?,
            token: need("TAB_RUNNER_TOKEN")?,
            data: std::env::var("TAB_DATA")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("/data")),
        })
    }
}

/// What the runner is set up with: the person's agent setup and the
/// connections that run here, built from the hub's secrets and rows.
#[derive(Debug, Clone, PartialEq)]
struct Setup {
    config: TabConfig,
    /// Environment the harnesses need (`ANTHROPIC_API_KEY`, ...).
    env: BTreeMap<String, String>,
    git_token: Option<String>,
}

fn setup(store: &Store, secrets: &[(String, String)], work: &Path) -> Setup {
    let mut config = TabConfig::default();
    let mut env = BTreeMap::new();
    let mut git_token = None;
    let mut agent: Option<AgentConfig> = None;
    let mut fields: BTreeMap<String, serde_json::Map<String, serde_json::Value>> = BTreeMap::new();
    for (name, value) in secrets {
        let parts: Vec<&str> = name.splitn(3, '/').collect();
        match parts.as_slice() {
            ["connection", id, field] => {
                fields
                    .entry((*id).to_string())
                    .or_default()
                    .insert((*field).to_string(), value.clone().into());
            }
            ["harness", _harness, var] => {
                env.insert((*var).to_string(), value.clone());
            }
            ["git", "token"] => git_token = Some(value.clone()),
            ["setup", "agent"] => match serde_json::from_str(value) {
                Ok(a) => agent = Some(a),
                Err(e) => {
                    tracing::warn!(error = %e, "the agent setup didn't parse; using defaults")
                }
            },
            _ => {}
        }
    }
    // Without git access of its own, agents push with GitHub's sign-in.
    if git_token.is_none() {
        git_token = fields
            .get("github")
            .and_then(|f| f.get("token"))
            .and_then(|t| t.as_str())
            .map(str::to_string);
    }
    let mut agent = agent.unwrap_or_default();
    // Paths from the person's computer mean nothing here.
    agent.command = None;
    agent.opencode_url = None;
    agent.workdir = Some(work.to_string_lossy().into_owned());
    config.agent = agent;
    config.connectors.clear();
    for c in store.cloud_connections().unwrap_or_default() {
        if !c.enabled {
            continue;
        }
        let mut settings = c.settings;
        if let Some(secret) = fields.remove(&c.id) {
            settings.extend(secret);
        }
        config.connectors.insert(
            c.id,
            ConnectorConfig {
                enabled: true,
                settings,
                runs_on: Default::default(),
            },
        );
    }
    Setup {
        config,
        env,
        git_token,
    }
}

/// Lets git (and `gh`) reach GitHub with the person's token.
fn use_git_token(token: &str, home: &Path) -> anyhow::Result<()> {
    std::fs::write(
        home.join(".git-credentials"),
        format!("https://x-access-token:{token}@github.com\n"),
    )?;
    let cfg = home.join(".gitconfig");
    std::fs::write(
        &cfg,
        "[credential]\n\thelper = store\n[user]\n\tname = tab cloud\n\temail = runner@tabqueue.com\n",
    )?;
    Ok(())
}

async fn secrets(cfg: &CloudConfig, token: &str) -> Vec<(String, String)> {
    let mut wait = 2;
    loop {
        match cloud::runner_secrets(cfg, token).await {
            Ok(s) => return s,
            Err(e) => {
                tracing::warn!(error = %e, retry_in = wait, "couldn't read the hub's secrets")
            }
        }
        tokio::time::sleep(Duration::from_secs(wait)).await;
        wait = (wait * 2).min(60);
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,tab_core=info".into()),
        )
        .init();
    let env = Env::from_env()?;
    let work = env.data.join("work");
    std::fs::create_dir_all(&work)?;
    let home = std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| env.data.clone());

    let store = Arc::new(Store::open(&env.data.join("tab.db"))?);
    let (events, _) = broadcast::channel::<EngineEvent>(1024);
    let hub = LocalHub::open(&env.data.join("hub.db"))?;
    // Everything goes to the cloud hub, never this container's hub.db.
    hub.expect_cloud();
    let runner_token = env.token.clone();
    let token: TokenSource = Arc::new(move || {
        let t = runner_token.clone();
        Box::pin(async move { Ok(t) })
    });
    let link = CloudLink::start(
        cloud_link::sync_url(&env.api, &env.hub),
        env.hub.clone(),
        token,
    );
    hub.follow(store.clone(), events.clone());
    let mut wait = 2;
    loop {
        let (h, s, l) = (hub.clone(), store.clone(), link.clone());
        match tokio::task::spawn_blocking(move || h.use_cloud(l, &s)).await? {
            Ok(()) => break,
            Err(e) => tracing::warn!(error = %e, retry_in = wait, "the hub isn't reachable yet"),
        }
        tokio::time::sleep(Duration::from_secs(wait)).await;
        wait = (wait * 2).min(60);
    }
    tracing::info!(hub = %env.hub, "cloud runner: syncing with the hub");

    let api = CloudConfig {
        api: env.api.clone(),
    };
    let mut current = setup(&store, &secrets(&api, &env.token).await, &work);
    apply_env(&current, &home);

    let agents = AgentHub::new(
        store.clone(),
        events.clone(),
        current.config.agent.clone(),
        env.data.join("agents"),
    );
    JobRunner::new(
        store.clone(),
        Arc::new(agents.clone()),
        hub.clone(),
        RunnerKind::Cloud,
        "tab cloud",
    )
    .start(CancellationToken::new());

    let gateway = Arc::new(Gateway::new(store.clone()));
    let start = |cfg: &TabConfig| -> EngineHandle {
        Engine::from_config(cfg, store.clone(), gateway.clone(), events.clone())
            .with_leases(hub.clone())
            .start()
    };
    let mut engine = start(&current.config);
    tracing::info!(
        connections = current.config.connectors.len(),
        "cloud runner: running"
    );

    let mut refresh = tokio::time::interval(REFRESH);
    refresh.tick().await;
    loop {
        tokio::select! {
            _ = refresh.tick() => {}
            _ = tokio::signal::ctrl_c() => break,
        }
        let next = setup(&store, &secrets(&api, &env.token).await, &work);
        if next != current {
            tracing::info!(
                connections = next.config.connectors.len(),
                "cloud runner: setup changed; restarting the engine"
            );
            apply_env(&next, &home);
            agents.set_config(next.config.agent.clone()).await;
            engine.shutdown();
            engine = start(&next.config);
            current = next;
        }
    }
    engine.shutdown();
    agents.shutdown();
    Ok(())
}

fn apply_env(s: &Setup, home: &Path) {
    for (k, v) in &s.env {
        // SAFETY: set before the harnesses start and between engine
        // restarts; nothing else in this process reads these concurrently
        // in a way that matters (each harness reads them when it spawns).
        unsafe { std::env::set_var(k, v) };
    }
    if let Some(t) = &s.git_token {
        unsafe {
            std::env::set_var("GH_TOKEN", t);
            std::env::set_var("GITHUB_TOKEN", t);
        }
        if let Err(e) = use_git_token(t, home) {
            tracing::warn!(error = %e, "couldn't set up git access");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn setup_comes_from_the_hubs_rows_and_secrets() {
        let store = Store::in_memory().unwrap();
        let now = chrono::Utc::now();
        store
            .cloud_connection_set("github", json!({ "repos": "acme/api" }).as_object(), now)
            .unwrap();
        let agent = json!({ "kind": "codex", "workdir": "/Users/v/code", "command": "/opt/codex" });
        let secrets = vec![
            ("connection/github/token".to_string(), "ghp_x".to_string()),
            (
                "connection/email/password".to_string(),
                "not set up here".to_string(),
            ),
            (
                "harness/codex/OPENAI_API_KEY".to_string(),
                "sk-x".to_string(),
            ),
            ("git/token".to_string(), "ghp_git".to_string()),
            ("setup/agent".to_string(), agent.to_string()),
        ];
        let s = setup(&store, &secrets, Path::new("/data/work"));
        let gh = &s.config.connectors["github"];
        assert_eq!(gh.settings["token"], "ghp_x");
        assert_eq!(gh.settings["repos"], "acme/api");
        assert!(
            !s.config.connectors.contains_key("email"),
            "only connections set to run here"
        );
        assert_eq!(s.env["OPENAI_API_KEY"], "sk-x");
        assert_eq!(s.git_token.as_deref(), Some("ghp_git"));
        assert_eq!(s.config.agent.kind, Some(tab_core::model::AgentKind::Codex));
        assert_eq!(s.config.agent.workdir.as_deref(), Some("/data/work"));
        assert_eq!(s.config.agent.command, None);
    }

    #[test]
    fn agents_push_with_githubs_sign_in_when_theres_no_git_access_of_their_own() {
        let store = Store::in_memory().unwrap();
        let secrets = vec![("connection/github/token".to_string(), "ghp_gh".to_string())];
        let s = setup(&store, &secrets, Path::new("/data/work"));
        assert_eq!(s.git_token.as_deref(), Some("ghp_gh"));
    }
}
