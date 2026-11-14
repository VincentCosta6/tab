//! Connections that run in tab cloud, and the hub's cloud runner.
//!
//! A person picks, per connection, whether it runs on this computer or in
//! tab cloud. In tab cloud, the hub's cloud runner (tab running headless next
//! to the hub, on paid plans) polls it, so it keeps running while every
//! computer is off. For that, the connection's settings go to the hub: the
//! plain ones as a synced `cloud_connections` row, the secret ones (tokens,
//! passwords) to the hub's secret store, which keeps them encrypted with the
//! hub's own key and hands them only to its cloud runner.
//!
//! The same store holds what the cloud runner's agents need: a harness's API
//! key (`harness/<id>/<ENV_VAR>`) and git access (`git/token`).
//!
//! MCP connections (Linear, Sentry, ...) go there once the person allows
//! them in tab cloud jobs ([`sync_mcp`]): the server, its keys, and a
//! sign-in made for tab cloud alone, which the runner refreshes and keeps
//! current in the hub. The names of the rest go too, so a cloud job that
//! needed one can say so.
//!
//! Nothing is sent that the person didn't enter for tab cloud: GitHub's
//! connection signs in with the GitHub CLI on a computer, but its whole
//! `gh` login (every repo, every scope) never goes to the cloud. To run
//! there it needs a token of its own, ideally fine-grained and read-only.

use std::collections::BTreeMap;

use tab_core::cloud::{self, mcp_secret, pb};
use tab_core::config::TabConfig;
use tab_core::connectors::registry;
use tab_core::model::SettingKind;
use tab_core::remote_mcp::RemoteMcp;
use tab_core::runner::RunsOn;
use tab_core::store::Store;
use tauri::State;
use tauri::ipc::Response;

use crate::account::{AccountState, CloudFailure};

/// Secret names a cloud runner may be given from Settings directly.
const RUNNER_SECRET_PREFIXES: &[&str] = &["harness/", "git/"];

/// A connector's settings split into plain ones and secret ones.
fn split(
    id: &str,
    settings: &serde_json::Map<String, serde_json::Value>,
) -> (
    serde_json::Map<String, serde_json::Value>,
    Vec<(String, String)>,
) {
    let secret: Vec<String> = registry()
        .into_iter()
        .map(|f| f.descriptor())
        .find(|d| d.id == id)
        .map(|d| {
            d.settings
                .into_iter()
                .filter(|s| s.kind == SettingKind::Secret)
                .map(|s| s.key)
                .collect()
        })
        .unwrap_or_default();
    let mut plain = serde_json::Map::new();
    let mut secrets = Vec::new();
    for (k, v) in settings {
        if secret.contains(k) {
            if let Some(s) = v.as_str().filter(|s| !s.is_empty()) {
                secrets.push((cloud::connection_secret(id, k), s.to_string()));
            }
        } else {
            plain.insert(k.clone(), v.clone());
        }
    }
    (plain, secrets)
}

/// The agent setup the cloud runner's harnesses use: this computer's, with
/// its paths left out (they mean nothing in tab cloud).
fn agent_setup(cfg: &TabConfig) -> String {
    let mut a = cfg.agent.clone();
    a.command = None;
    a.workdir = None;
    serde_json::to_string(&a).unwrap_or_default()
}

/// The agent setup last sent, so a save that didn't change it sends nothing.
static SENT_SETUP: std::sync::Mutex<Option<(String, String)>> = std::sync::Mutex::new(None);

/// What was last sent of the MCP connections: the hub, each connection's
/// server and keys (as sent), and the list of the rest.
#[derive(Clone, Default, PartialEq)]
struct SentMcp {
    hub: String,
    servers: BTreeMap<String, (String, Option<String>)>,
    elsewhere: String,
}

static SENT_MCP: std::sync::Mutex<Option<SentMcp>> = std::sync::Mutex::new(None);

/// The MCP part of [`sync_connections`]: secrets to put and remove so the
/// hub has the connections allowed in tab cloud jobs (with their keys and
/// any new cloud sign-in), and the names of the rest. Returns them with
/// the ids whose cloud sign-in goes up, and what to remember once sent.
async fn sync_mcp(
    account: &AccountState,
    remote: &RemoteMcp,
    cfg: &TabConfig,
    hub: &str,
) -> Result<(Vec<(String, String)>, Vec<String>, Vec<String>, SentMcp), CloudFailure> {
    let plan = tab_core::remote_mcp::cloud_plan_here(&cfg.mcp_servers);
    let mut now = SentMcp {
        hub: hub.to_string(),
        elsewhere: serde_json::to_string(&plan.elsewhere).unwrap_or_default(),
        ..Default::default()
    };
    for (id, c) in &plan.servers {
        let server = serde_json::to_string(c).unwrap_or_default();
        let keys = Some(remote.secrets(id))
            .filter(|k| !k.is_empty())
            .and_then(|k| serde_json::to_string(&k).ok());
        now.servers.insert(id.clone(), (server, keys));
    }
    let before = SENT_MCP
        .lock()
        .ok()
        .and_then(|g| g.clone())
        .filter(|s| s.hub == hub);
    let mut put = Vec::new();
    let mut remove = Vec::new();
    let mut signed_in = Vec::new();
    // A cloud sign-in made on this computer and not sent yet.
    for id in plan.servers.keys() {
        if let Some(s) = remote.cloud_sign_in(id) {
            put.push((mcp_secret::name(id, mcp_secret::OAUTH), s));
            signed_in.push(id.clone());
        }
    }
    let fields = [mcp_secret::SERVER, mcp_secret::KEYS, mcp_secret::OAUTH];
    match &before {
        Some(b) => {
            for id in b.servers.keys().filter(|id| !now.servers.contains_key(*id)) {
                remove.extend(fields.iter().map(|f| mcp_secret::name(id, f)));
            }
        }
        // First time since this app started: whatever the hub has for
        // connections that aren't allowed any more goes.
        None => {
            let token = account.token().await?;
            for name in cloud::secret_names(account.cloud(), &token, hub).await? {
                if let Some((id, _)) = mcp_secret::parse(&name)
                    && !now.servers.contains_key(id)
                {
                    remove.push(name);
                }
            }
        }
    }
    for (id, (server, keys)) in &now.servers {
        let old = before.as_ref().and_then(|b| b.servers.get(id));
        if old.map(|o| &o.0) != Some(server) {
            put.push((mcp_secret::name(id, mcp_secret::SERVER), server.clone()));
        }
        if old.map(|o| &o.1) != Some(keys) {
            match keys {
                Some(k) => put.push((mcp_secret::name(id, mcp_secret::KEYS), k.clone())),
                None => remove.push(mcp_secret::name(id, mcp_secret::KEYS)),
            }
        }
    }
    if before.as_ref().map(|b| &b.elsewhere) != Some(&now.elsewhere) {
        put.push((mcp_secret::ELSEWHERE.to_string(), now.elsewhere.clone()));
    }
    Ok((put, remove, signed_in, now))
}

/// Brings the hub in line with which connections run in tab cloud: their
/// plain settings as synced rows, their secrets in the hub's store, and
/// nothing left behind for connections that went back to a computer. The
/// same for MCP connections allowed in tab cloud jobs.
pub async fn sync_connections(
    account: &AccountState,
    store: &Store,
    remote: &RemoteMcp,
    cfg: &TabConfig,
) -> Result<(), CloudFailure> {
    let Some(hub) = cfg.hub.cloud_hub.clone() else {
        return Ok(());
    };
    let now = chrono::Utc::now();
    let mut put = Vec::new();
    let mut remove = Vec::new();
    let in_cloud: Vec<String> = store
        .cloud_connections()
        .map_err(CloudFailure::internal)?
        .into_iter()
        .map(|c| c.id)
        .collect();
    for f in registry() {
        let id = f.descriptor().id;
        let c = cfg.connectors.get(&id);
        let cloud = c.is_some_and(|c| c.enabled && c.runs_on == RunsOn::Cloud);
        let (plain, secrets) = match c {
            Some(c) => split(&id, &c.settings),
            None => Default::default(),
        };
        if cloud {
            store
                .cloud_connection_set(&id, Some(&plain), now)
                .map_err(CloudFailure::internal)?;
            put.extend(secrets);
        } else if in_cloud.contains(&id) {
            store
                .cloud_connection_set(&id, None, now)
                .map_err(CloudFailure::internal)?;
            for d in registry()
                .into_iter()
                .map(|f| f.descriptor())
                .filter(|d| d.id == id)
            {
                for s in d
                    .settings
                    .into_iter()
                    .filter(|s| s.kind == SettingKind::Secret)
                {
                    remove.push(cloud::connection_secret(&id, &s.key));
                }
            }
        }
    }
    let setup = agent_setup(cfg);
    let sent = SENT_SETUP.lock().ok().and_then(|g| g.clone());
    let new_setup = sent.as_ref() != Some(&(hub.clone(), setup.clone()));
    if new_setup {
        put.push((cloud::AGENT_SETUP_SECRET.to_string(), setup.clone()));
    }
    let (mcp_put, mcp_remove, signed_in, mcp_sent) = sync_mcp(account, remote, cfg, &hub).await?;
    let mcp_changed = !mcp_put.is_empty() || !mcp_remove.is_empty();
    put.extend(mcp_put);
    remove.extend(mcp_remove);
    if put.is_empty() && remove.is_empty() {
        if let Ok(mut g) = SENT_MCP.lock() {
            *g = Some(mcp_sent);
        }
        return Ok(());
    }
    let token = account.token().await?;
    cloud::put_secrets(account.cloud(), &token, &hub, put, remove).await?;
    if let Ok(mut g) = SENT_SETUP.lock() {
        *g = Some((hub.clone(), setup));
    }
    if let Ok(mut g) = SENT_MCP.lock() {
        *g = Some(mcp_sent);
    }
    // The hub has them now: the runner refreshes them from here on.
    for id in signed_in {
        remote.clear_cloud_sign_in(&id);
    }
    if mcp_changed {
        let _ = cloud::wake_runner(account.cloud(), &token, &hub).await;
    }
    // Wake the runner so it picks the change up now.
    if cfg
        .connectors
        .values()
        .any(|c| c.enabled && c.runs_on == RunsOn::Cloud)
    {
        let _ = cloud::wake_runner(account.cloud(), &token, &hub).await;
    }
    Ok(())
}

/// Before this computer's work leaves tab cloud: nothing of it keeps running
/// there. Connections stop running in the cloud, and every secret the hub
/// keeps for its runner (sign-ins, harness keys, git access) is removed, so
/// a runner still up has nothing to work with and stops when idle.
pub async fn forget_cloud(
    account: &AccountState,
    store: &Store,
    remote: &RemoteMcp,
    cfg: &TabConfig,
) -> Result<(), CloudFailure> {
    let Some(hub) = cfg.hub.cloud_hub.clone() else {
        return Ok(());
    };
    let mut local = cfg.clone();
    for c in local.connectors.values_mut() {
        c.runs_on = RunsOn::ThisComputer;
    }
    for c in local.mcp_servers.values_mut() {
        c.cloud_jobs = false;
    }
    sync_connections(account, store, remote, &local).await?;
    let token = account.token().await?;
    let names = cloud::secret_names(account.cloud(), &token, &hub).await?;
    if !names.is_empty() {
        cloud::put_secrets(account.cloud(), &token, &hub, vec![], names).await?;
    }
    if let Ok(mut g) = SENT_SETUP.lock() {
        *g = None;
    }
    if let Ok(mut g) = SENT_MCP.lock() {
        *g = None;
    }
    Ok(())
}

/// Where one MCP connection stands for tab cloud jobs.
#[derive(serde::Serialize)]
pub struct McpCloudState {
    pub id: String,
    /// The person allowed it ("Allow in cloud").
    pub allowed: bool,
    /// `off`, `ready`, `sign_in` (needs its sign-in for tab cloud),
    /// `cant_run` (only works on a computer), `no_hub` (the hub isn't in
    /// tab cloud yet) or `unknown` (tab cloud couldn't be asked).
    pub state: &'static str,
    pub why: Option<String>,
}

/// Where each MCP connection stands for tab cloud jobs. Asks the hub which
/// cloud sign-ins it keeps (names only).
pub async fn mcp_cloud_states(
    account: &AccountState,
    remote: &RemoteMcp,
    cfg: &TabConfig,
) -> Vec<McpCloudState> {
    let names = match &cfg.hub.cloud_hub {
        Some(hub) if cfg.mcp_servers.values().any(|c| c.cloud_jobs) => {
            match account.token().await {
                Ok(t) => cloud::secret_names(account.cloud(), &t, hub)
                    .await
                    .map_err(|e| e.message),
                Err(e) => Err(e.message().to_string()),
            }
        }
        _ => Ok(Vec::new()),
    };
    let fits: BTreeMap<String, tab_core::remote_mcp::CloudFit> = remote
        .infos()
        .into_iter()
        .map(|i| (i.id, i.cloud))
        .collect();
    cfg.mcp_servers
        .iter()
        .map(|(id, c)| {
            let fit = fits.get(id);
            let (state, why) = if cfg.hub.cloud_hub.is_none() {
                (
                    "no_hub",
                    Some("Move your hub to tab cloud first (Settings → Hub).".to_string()),
                )
            } else if fit.is_some_and(|f| !f.portable) {
                ("cant_run", fit.and_then(|f| f.why_not.clone()))
            } else if !c.cloud_jobs {
                ("off", None)
            } else if !c.sign_in {
                ("ready", None)
            } else {
                match &names {
                    Ok(n)
                        if n.contains(&mcp_secret::name(id, mcp_secret::OAUTH))
                            || remote.cloud_sign_in(id).is_some() =>
                    {
                        ("ready", None)
                    }
                    Ok(_) => ("sign_in", None),
                    Err(e) => ("unknown", Some(e.clone())),
                }
            };
            McpCloudState {
                id: id.clone(),
                allowed: c.cloud_jobs,
                state,
                why,
            }
        })
        .collect()
}

/// Sets where a connection runs and saves it.
#[tauri::command]
pub async fn connection_runs_on(
    app: State<'_, crate::AppState>,
    account: State<'_, AccountState>,
    connection: String,
    runs_on: RunsOn,
) -> Result<TabConfig, CloudFailure> {
    let mut next = app.config.lock().await.clone();
    if runs_on == RunsOn::Cloud && next.hub.cloud_hub.is_none() {
        return Err(cloud::CloudError::new(
            pb::ErrorCode::BadRequest,
            "Move your hub to tab cloud first (Settings → Hub), then a connection can run there.",
        )
        .into());
    }
    let Some(c) = next.connectors.get_mut(&connection) else {
        return Err(cloud::CloudError::new(
            pb::ErrorCode::BadRequest,
            format!("{connection} isn't set up yet"),
        )
        .into());
    };
    if runs_on == RunsOn::Cloud
        && let Some(why) = needs_own_sign_in(&connection, c)
    {
        return Err(cloud::CloudError::new(pb::ErrorCode::BadRequest, why).into());
    }
    c.runs_on = runs_on;
    // Send the secrets before this app stops running it, so nothing is
    // missed if the cloud can't be reached: the save fails instead.
    sync_connections(&account, &app.store, app.hub.remote_mcp(), &next).await?;
    let saved = app.apply(next).await.map_err(CloudFailure::internal)?;
    Ok(crate::masked(&saved))
}

/// Why a connection can't run in tab cloud as it's set up: GitHub signs in
/// with this computer's GitHub CLI unless it has a token of its own, and
/// that login is never sent.
fn needs_own_sign_in(
    connection: &str,
    c: &tab_core::config::ConnectorConfig,
) -> Option<&'static str> {
    let has_token = c
        .settings
        .get("token")
        .and_then(|t| t.as_str())
        .is_some_and(|t| !t.trim().is_empty());
    if connection == "email" && c.str("auth").is_some() {
        return Some(
            "Email signed in with Google or Microsoft stays on this computer for now: tab cloud can't refresh that sign-in yet. Use an app password to run it in tab cloud.",
        );
    }
    (connection == "github" && !has_token).then_some(
        "To run in tab cloud, GitHub needs a token of its own: tab doesn't send this computer's GitHub CLI sign-in, which reaches every repo you can. Add a fine-grained token with read access to the repos you follow in this connection's settings, then move it.",
    )
}

/// A runner as the header and session views show it.
#[derive(serde::Serialize)]
pub struct RunnerView {
    #[serde(flatten)]
    caps: tab_core::runner::RunnerCaps,
    awake: bool,
    this_app: bool,
}

/// Every runner the hub knows (this computer, others, the cloud runner),
/// whether each is awake, and which one is this app.
#[tauri::command]
pub async fn runners(app: State<'_, crate::AppState>) -> Result<Vec<RunnerView>, String> {
    let me = app.store.device_id().map_err(|e| e.to_string())?;
    let now = chrono::Utc::now();
    Ok(app
        .store
        .runners()
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|caps| RunnerView {
            awake: caps.awake(now),
            this_app: caps.id == me,
            caps,
        })
        .collect())
}

/// Jobs workflows handed to runners, newest first.
#[tauri::command]
pub async fn jobs_recent(
    app: State<'_, crate::AppState>,
) -> Result<Vec<tab_core::runner::Job>, String> {
    app.store.jobs_recent(50).map_err(|e| e.to_string())
}

/// The hub's cloud runner: a `tab.cloud.v1.RunnerStatus`.
#[tauri::command]
pub async fn cloud_runner_status(
    app: State<'_, crate::AppState>,
    account: State<'_, AccountState>,
) -> Result<Response, CloudFailure> {
    let hub = moved_hub(&app).await?;
    let token = account.token().await?;
    Ok(crate::account::bytes(
        cloud::runner_status(account.cloud(), &token, &hub).await?,
    ))
}

/// Wakes the hub's cloud runner: a `tab.cloud.v1.RunnerStatus`.
#[tauri::command]
pub async fn cloud_runner_wake(
    app: State<'_, crate::AppState>,
    account: State<'_, AccountState>,
) -> Result<Response, CloudFailure> {
    let hub = moved_hub(&app).await?;
    let token = account.token().await?;
    Ok(crate::account::bytes(
        cloud::wake_runner(account.cloud(), &token, &hub).await?,
    ))
}

/// The names of the secrets the hub keeps (never their values).
#[tauri::command]
pub async fn cloud_secret_names(
    app: State<'_, crate::AppState>,
    account: State<'_, AccountState>,
) -> Result<Vec<String>, CloudFailure> {
    let hub = moved_hub(&app).await?;
    let token = account.token().await?;
    Ok(cloud::secret_names(account.cloud(), &token, &hub).await?)
}

/// Stores (or with no value, removes) a secret for the cloud runner's
/// agents: a harness's API key, or git access.
#[tauri::command]
pub async fn cloud_secret_set(
    app: State<'_, crate::AppState>,
    account: State<'_, AccountState>,
    name: String,
    value: Option<String>,
) -> Result<Vec<String>, CloudFailure> {
    if !RUNNER_SECRET_PREFIXES.iter().any(|p| name.starts_with(p)) {
        return Err(cloud::CloudError::new(
            pb::ErrorCode::BadRequest,
            "Only a harness key or git access can be set here.",
        )
        .into());
    }
    let hub = moved_hub(&app).await?;
    let token = account.token().await?;
    let value = value.filter(|v| !v.trim().is_empty());
    let (put, remove) = match value {
        Some(v) => (vec![(name, v.trim().to_string())], vec![]),
        None => (vec![], vec![name]),
    };
    Ok(cloud::put_secrets(account.cloud(), &token, &hub, put, remove).await?)
}

async fn moved_hub(app: &crate::AppState) -> Result<String, CloudFailure> {
    app.config
        .lock()
        .await
        .hub
        .cloud_hub
        .clone()
        .ok_or_else(|| {
            cloud::CloudError::new(
                pb::ErrorCode::NotFound,
                "This computer's work isn't in tab cloud yet.",
            )
            .into()
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_agent_setup_leaves_this_computers_paths_out() {
        let mut cfg = TabConfig::default();
        cfg.agent.workdir = Some("/Users/v/code".into());
        cfg.agent.command = Some("/opt/claude".into());
        let s: serde_json::Value = serde_json::from_str(&agent_setup(&cfg)).unwrap();
        assert!(s["workdir"].is_null() && s["command"].is_null());
    }

    #[test]
    fn github_needs_its_own_token_to_run_in_the_cloud() {
        let mut c = tab_core::config::ConnectorConfig::default();
        assert!(needs_own_sign_in("github", &c).is_some());
        c.settings.insert("token".into(), json!("github_pat_x"));
        assert!(needs_own_sign_in("github", &c).is_none());
        assert!(
            needs_own_sign_in("email", &tab_core::config::ConnectorConfig::default()).is_none()
        );
    }

    #[test]
    fn secret_fields_are_split_from_the_rest() {
        let settings = json!({ "token": "ghp_x", "repos": "acme/api", "empty": "" });
        let (plain, secrets) = split("github", settings.as_object().unwrap());
        assert_eq!(
            secrets,
            [("connection/github/token".to_string(), "ghp_x".to_string())]
        );
        assert_eq!(plain.get("repos"), Some(&json!("acme/api")));
        assert!(!plain.contains_key("token"));
    }
}
