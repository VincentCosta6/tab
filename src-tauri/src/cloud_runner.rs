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
//! Nothing is sent that the person didn't enter for tab cloud: GitHub's
//! connection signs in with the GitHub CLI on a computer, but its whole
//! `gh` login (every repo, every scope) never goes to the cloud. To run
//! there it needs a token of its own, ideally fine-grained and read-only.

use tab_core::cloud::{self, pb};
use tab_core::config::TabConfig;
use tab_core::connectors::registry;
use tab_core::model::SettingKind;
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
    a.opencode_url = None;
    a.workdir = None;
    serde_json::to_string(&a).unwrap_or_default()
}

/// The agent setup last sent, so a save that didn't change it sends nothing.
static SENT_SETUP: std::sync::Mutex<Option<(String, String)>> = std::sync::Mutex::new(None);

/// Brings the hub in line with which connections run in tab cloud: their
/// plain settings as synced rows, their secrets in the hub's store, and
/// nothing left behind for connections that went back to a computer.
pub async fn sync_connections(
    account: &AccountState,
    store: &Store,
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
    if put.is_empty() && remove.is_empty() {
        return Ok(());
    }
    let token = account.token().await?;
    cloud::put_secrets(account.cloud(), &token, &hub, put, remove).await?;
    if let Ok(mut g) = SENT_SETUP.lock() {
        *g = Some((hub.clone(), setup));
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
    cfg: &TabConfig,
) -> Result<(), CloudFailure> {
    let Some(hub) = cfg.hub.cloud_hub.clone() else {
        return Ok(());
    };
    let mut local = cfg.clone();
    for c in local.connectors.values_mut() {
        c.runs_on = RunsOn::ThisComputer;
    }
    sync_connections(account, store, &local).await?;
    let token = account.token().await?;
    let names = cloud::secret_names(account.cloud(), &token, &hub).await?;
    if !names.is_empty() {
        cloud::put_secrets(account.cloud(), &token, &hub, vec![], names).await?;
    }
    if let Ok(mut g) = SENT_SETUP.lock() {
        *g = None;
    }
    Ok(())
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
    sync_connections(&account, &app.store, &next).await?;
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
