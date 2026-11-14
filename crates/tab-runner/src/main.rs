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
//! - It fronts the MCP connections (Linear, Sentry, ...) the person allowed
//!   in tab cloud jobs, with the cloud column's choice for each tool and a
//!   sign-in of the cloud's own, which it refreshes and writes back to the
//!   hub. Connections that aren't here get a stand-in, so a job that needs
//!   one can say so and the person gets a one-click fix.
//!
//! Configured by environment (set by the hub when it starts the container):
//! `TAB_CLOUD_URL`, `TAB_HUB_ID`, `TAB_RUNNER_TOKEN`, and `TAB_DATA` (a
//! writable folder, `/data` by default).
//!
//! Agents here run prompts that may come from anyone's PR or email, so the
//! runner keeps everything worth stealing away from them:
//!
//! - Every agent and command runs in tab's sandbox (bubblewrap, in its own
//!   PID namespace), and the runner starts none at all if the sandbox isn't
//!   working. tab's data folder (`tab.db`, `hub.db`) is hidden from them.
//! - The hub's secrets stay in this process's memory. Harness keys go to
//!   each agent's own environment when it starts; the git token goes only
//!   to the runner's own git commands, and agents get the review policy, so
//!   no GitHub token reaches them. The runner token leaves the environment
//!   as soon as it's read, and the process isn't dumpable, so nothing of the
//!   same user can read it from `/proc`.
//! - It goes by the device id its hub gives it (`runner-<hub id>`) and
//!   starts from an empty data folder each time, keeping only clones.

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
use tab_core::mcp::{EngineAccess, McpServer};
use tab_core::remote_mcp::{Elsewhere, RemoteMcp, RemoteMcpConfig, Secrets};
use tab_core::runner::RunnerKind;
use tab_core::sandbox::{self, Sandbox, SandboxConfig};
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
    /// Keys of the MCP connections allowed here (in `config.mcp_servers`).
    mcp_keys: BTreeMap<String, Secrets>,
    /// The person's connections that aren't here.
    elsewhere: Vec<Elsewhere>,
}

fn setup(store: &Store, secrets: &[(String, String)], work: &Path) -> Setup {
    let mut config = TabConfig::default();
    let mut env = BTreeMap::new();
    let mut git_token = None;
    let mut agent: Option<AgentConfig> = None;
    let mut fields: BTreeMap<String, serde_json::Map<String, serde_json::Value>> = BTreeMap::new();
    let mut mcp: BTreeMap<String, RemoteMcpConfig> = BTreeMap::new();
    let mut mcp_keys = BTreeMap::new();
    let mut elsewhere: Vec<Elsewhere> = Vec::new();
    for (name, value) in secrets {
        if name == cloud::mcp_secret::ELSEWHERE {
            match serde_json::from_str(value) {
                Ok(e) => elsewhere = e,
                Err(e) => {
                    tracing::warn!(error = %e, "the list of connections elsewhere didn't parse")
                }
            }
            continue;
        }
        if let Some((id, field)) = cloud::mcp_secret::parse(name) {
            match field {
                cloud::mcp_secret::SERVER => match serde_json::from_str::<RemoteMcpConfig>(value) {
                    Ok(c) => {
                        mcp.insert(id.to_string(), c);
                    }
                    Err(e) => tracing::warn!(id, error = %e, "an MCP connection didn't parse"),
                },
                cloud::mcp_secret::KEYS => {
                    if let Ok(k) = serde_json::from_str::<Secrets>(value) {
                        mcp_keys.insert(id.to_string(), k);
                    }
                }
                // Sign-ins are seeded apart (`sign_ins`): the runner
                // rewrites them, which isn't a change of setup.
                _ => {}
            }
            continue;
        }
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
    // Git access is only what was given as git access (`git/token`), never
    // the GitHub connection's sign-in, which is the person's whole login.
    let mut agent = agent.unwrap_or_default();
    // Paths from the person's computer mean nothing here.
    agent.command = None;
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
    // Only connections that are on and can run here; the rest are known
    // only as elsewhere.
    let home = std::env::var_os("HOME").map(PathBuf::from);
    for (id, mut c) in mcp {
        if !c.enabled
            || !c.cloud_jobs
            || !tab_core::remote_mcp::cloud_fit(&c, home.as_deref()).portable
        {
            continue;
        }
        c.tools.clear();
        config.mcp_servers.insert(id, c);
    }
    mcp_keys.retain(|id, _| config.mcp_servers.contains_key(id));
    elsewhere.retain(|e| !config.mcp_servers.contains_key(&e.id));
    Setup {
        config,
        env,
        git_token,
        mcp_keys,
        elsewhere,
    }
}

/// The cloud sign-ins of MCP connections, by id (JSON `SignIn`).
fn sign_ins(secrets: &[(String, String)]) -> BTreeMap<String, String> {
    secrets
        .iter()
        .filter_map(|(name, value)| {
            let (id, field) = cloud::mcp_secret::parse(name)?;
            (field == cloud::mcp_secret::OAUTH).then(|| (id.to_string(), value.clone()))
        })
        .collect()
}

/// Puts the MCP connections allowed here in place.
fn apply_mcp(remote: &RemoteMcp, s: &Setup) {
    remote.set_servers(s.config.mcp_servers.clone());
    for id in s.config.mcp_servers.keys() {
        let keys = s.mcp_keys.get(id).cloned().unwrap_or_default();
        if let Err(e) = remote.set_secrets(id, &keys) {
            tracing::warn!(id, error = %e, "couldn't keep an MCP connection's keys");
        }
    }
    remote.set_elsewhere(s.elsewhere.clone());
}

/// The sign-ins this runner last saw or wrote, so one the hub still has
/// from before a refresh here isn't put back over the newer one.
type Seen = Arc<std::sync::Mutex<BTreeMap<String, String>>>;

/// Seeds sign-ins that are new, or that the person redid on a computer.
fn seed(remote: &RemoteMcp, seen: &Seen, all: &BTreeMap<String, String>) {
    let mut seen = seen.lock().unwrap();
    for (id, json) in all {
        if seen.get(id) == Some(json) {
            continue;
        }
        match remote.seed_sign_in(id, json) {
            Ok(()) => {
                seen.insert(id.clone(), json.clone());
            }
            Err(e) => tracing::warn!(id, error = %e, "an MCP sign-in didn't parse"),
        }
    }
}

/// What tab's MCP server needs from the engine. A cloud runner's fronts
/// only connections, so it needs nothing.
struct NoEngine;

#[async_trait::async_trait]
impl EngineAccess for NoEngine {
    async fn create_task(
        &self,
        _: tab_core::model::NewTask,
    ) -> anyhow::Result<tab_core::QueueItem> {
        anyhow::bail!("tasks can't be made from tab cloud")
    }
    async fn preview(&self, _: &str) -> anyhow::Result<tab_core::model::Preview> {
        anyhow::bail!("previews aren't available in tab cloud")
    }
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

/// Everything in the data folder but the clones: the last run's `tab.db`
/// and `hub.db` hold every synced record, and the hub is the source of
/// truth anyway.
fn start_clean(data: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(data)?;
    for e in std::fs::read_dir(data)? {
        let e = e?;
        if e.file_name() == "work" {
            continue;
        }
        let p = e.path();
        if e.file_type()?.is_dir() {
            std::fs::remove_dir_all(&p)?;
        } else {
            std::fs::remove_file(&p)?;
        }
    }
    Ok(())
}

fn main() -> anyhow::Result<()> {
    // Inside an agent's sandbox: the bridge to tab's proxy.
    let mut args = std::env::args_os().skip(1);
    if args
        .next()
        .is_some_and(|a| a == sandbox::bridge::SUBCOMMAND)
    {
        std::process::exit(sandbox::bridge::main(args.collect()));
    }
    sandbox::harden_process(true);
    tab_core::limits::raise_open_file_limit();
    let env = Env::from_env()?;
    // SAFETY: no other thread has started yet.
    unsafe { std::env::remove_var("TAB_RUNNER_TOKEN") };
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run(env))
}

async fn run(env: Env) -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,tab_core=info".into()),
        )
        .init();
    start_clean(&env.data)?;
    let work = env.data.join("work");
    std::fs::create_dir_all(&work)?;

    // Nothing starts outside the sandbox here, and tab's own files in the
    // data folder are hidden from whatever does.
    let sb = Sandbox::new(
        SandboxConfig::default(),
        Some(env.data.clone()),
        Some(env.data.clone()),
    );
    sb.require_enforcement(true);
    if let Ok(exe) = std::env::current_exe() {
        sandbox::bridge::set_helper(exe);
    }
    let sb = Arc::new(sb);
    sandbox::install(sb.clone());
    let status = sb.status().await;
    if !status.enforced {
        tracing::error!(
            backend = ?status.backend,
            "tab's sandbox isn't working in this container; agent jobs and workflow commands will fail"
        );
    }

    let store = Arc::new(Store::open(&env.data.join("tab.db"))?);
    store.set_device_id(&tab_hub::runner_device(&env.hub))?;
    let (events, _) = broadcast::channel::<EngineEvent>(1024);
    let hub = LocalHub::open(&env.data.join("hub.db"))?;
    // Everything goes to the cloud hub, never this container's hub.db.
    hub.expect_cloud(&env.hub);
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
    let first = secrets(&api, &env.token).await;
    let mut current = setup(&store, &first, &work);
    sb.set_spawn_env(current.env.clone());

    let agents = AgentHub::new(
        store.clone(),
        events.clone(),
        current.config.agent.clone(),
        env.data.join("agents"),
    );
    // Every job here works on a clone of someone's repo.
    agents.review_under(&work);

    // The MCP connections the person allowed here, through tab's MCP
    // server on this container's loopback. Their sign-ins live in the data
    // folder, which agents can't read.
    let remote = agents.remote_mcp().clone();
    remote.set_in_cloud(true);
    let seen: Seen = Default::default();
    {
        let (api, token, seen) = (api.clone(), env.token.clone(), seen.clone());
        remote.set_on_saved(Arc::new(move |id: &str, json: &str| {
            seen.lock().unwrap().insert(id.to_string(), json.to_string());
            let name = cloud::mcp_secret::name(id, cloud::mcp_secret::OAUTH);
            let (api, token) = (api.clone(), token.clone());
            let (json, id) = (json.to_string(), id.to_string());
            tokio::spawn(async move {
                if let Err(e) = cloud::runner_put_secrets(&api, &token, vec![(name, json)]).await {
                    tracing::warn!(id, error = %e, "couldn't keep a refreshed MCP sign-in in the hub; this runner keeps using it until it stops");
                }
            });
        }));
    }
    apply_mcp(&remote, &current);
    seed(&remote, &seen, &sign_ins(&first));
    let mcp_token = uuid::Uuid::new_v4().simple().to_string();
    let server = Arc::new(McpServer {
        store: store.clone(),
        hub: agents.clone(),
        engine: Arc::new(NoEngine),
        events: events.clone(),
        token: mcp_token.clone(),
    });
    match tab_core::mcp::serve(server, 47_800).await {
        Ok(url) => agents.set_mcp(tab_core::agents::McpEndpoint {
            url,
            token: mcp_token,
        }),
        Err(e) => {
            tracing::error!(error = %e, "couldn't start the connections proxy; jobs get no connections")
        }
    }
    let jobs = JobRunner::new(
        store.clone(),
        Arc::new(agents.clone()),
        hub.clone(),
        RunnerKind::Cloud,
        "tab cloud",
    );
    jobs.set_git_token(current.git_token.clone());
    jobs.clone().start(CancellationToken::new());

    let gateway = Arc::new(Gateway::new(store.clone()));
    let start = |cfg: &TabConfig| -> EngineHandle {
        Engine::from_config(cfg, store.clone(), gateway.clone(), events.clone())
            .with_mcp(cfg, &remote)
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
        let all = secrets(&api, &env.token).await;
        let next = setup(&store, &all, &work);
        seed(&remote, &seen, &sign_ins(&all));
        if next != current {
            tracing::info!(
                connections = next.config.connectors.len(),
                "cloud runner: setup changed; restarting the engine"
            );
            sb.set_spawn_env(next.env.clone());
            jobs.set_git_token(next.git_token.clone());
            agents.set_config(next.config.agent.clone()).await;
            apply_mcp(&remote, &next);
            engine.shutdown();
            engine = start(&next.config);
            current = next;
        }
    }
    engine.shutdown();
    agents.shutdown();
    Ok(())
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
    fn mcp_connections_come_from_the_hubs_secrets() {
        use tab_core::remote_mcp::ToolPolicy;
        use tab_core::runner::GapKind;
        let store = Store::in_memory().unwrap();
        let server = |cloud_jobs: bool, url: &str| {
            json!({ "name": "Linear", "url": url, "enabled": true, "sign_in": true,
                    "cloud_jobs": cloud_jobs, "cloud_tools": { "create_issue": "allow" },
                    "tools": { "x": "allow" } })
            .to_string()
        };
        let secrets = vec![
            (
                "connection/mcp.linear/server".to_string(),
                server(true, "https://mcp.linear.app/mcp"),
            ),
            (
                "connection/mcp.linear/oauth".to_string(),
                "{\"signed\":1}".to_string(),
            ),
            (
                "connection/mcp.linear/keys".to_string(),
                json!({ "headers": { "X-Key": "k" } }).to_string(),
            ),
            // Sent before the person took it back out of the cloud.
            (
                "connection/mcp.old/server".to_string(),
                server(false, "https://mcp.old.dev/mcp"),
            ),
            // Allowed, but on a private address.
            (
                "connection/mcp.dev/server".to_string(),
                server(true, "http://10.0.0.5/mcp"),
            ),
            (
                "connection/mcp/elsewhere".to_string(),
                json!([{ "id": "notion", "name": "Notion", "why": "not_in_cloud" },
                       { "id": "linear", "name": "Linear", "why": "not_in_cloud" }])
                .to_string(),
            ),
        ];
        let s = setup(&store, &secrets, Path::new("/data/work"));
        assert_eq!(s.config.mcp_servers.keys().collect::<Vec<_>>(), ["linear"]);
        let l = &s.config.mcp_servers["linear"];
        assert!(l.tools.is_empty(), "the chat column stays on the computer");
        assert_eq!(l.cloud_tools["create_issue"], ToolPolicy::Allow);
        assert_eq!(s.mcp_keys["linear"].headers["X-Key"], "k");
        // What's here isn't also a stand-in.
        assert_eq!(
            s.elsewhere
                .iter()
                .map(|e| (e.id.as_str(), e.why))
                .collect::<Vec<_>>(),
            [("notion", GapKind::NotInCloud)]
        );
        // Sign-ins are seeded apart, so the runner writing one back isn't a
        // change of setup.
        assert_eq!(sign_ins(&secrets)["linear"], "{\"signed\":1}");
        let mut later = secrets.clone();
        later[1].1 = "{\"signed\":2}".into();
        assert_eq!(setup(&store, &later, Path::new("/data/work")), s);
        assert!(!s.config.connectors.contains_key("mcp.linear"));
    }

    #[test]
    fn git_access_is_never_the_github_connections_sign_in() {
        let store = Store::in_memory().unwrap();
        let secrets = vec![("connection/github/token".to_string(), "ghp_gh".to_string())];
        let s = setup(&store, &secrets, Path::new("/data/work"));
        assert_eq!(s.git_token, None);
    }

    #[test]
    fn each_start_keeps_only_the_clones() {
        let d = std::env::temp_dir().join(format!("tab-runner-clean-{}", std::process::id()));
        std::fs::create_dir_all(d.join("work/repos/acme__api")).unwrap();
        std::fs::create_dir_all(d.join("agents/sessions")).unwrap();
        std::fs::write(d.join("tab.db"), "old").unwrap();
        std::fs::write(d.join("hub.db-wal"), "old").unwrap();
        start_clean(&d).unwrap();
        let left: Vec<_> = std::fs::read_dir(&d)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(left, vec![std::ffi::OsString::from("work")]);
        assert!(d.join("work/repos/acme__api").is_dir());
        std::fs::remove_dir_all(&d).unwrap();
    }
}
