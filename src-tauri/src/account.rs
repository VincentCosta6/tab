//! Signing in to a tab account (Supabase, with Apple or Google). The session
//! stays in `account.json` beside `config.json`; the UI only sees who's
//! signed in.

use std::path::PathBuf;
use std::sync::Mutex;

use prost::Message;
use tab_core::account::{
    self, AccountStatus, AccountView, CancellationToken, Provider, SupabaseConfig,
};
use tab_core::cloud::{self, CloudConfig, CloudError};
use tab_core::cloud_link::{self, CloudLink, TokenSource};
use tauri::State;
use tauri::ipc::Response;

pub struct AccountState {
    path: PathBuf,
    supabase: Option<SupabaseConfig>,
    cloud: CloudConfig,
    /// The sign-in waiting on the browser, if any.
    pending: Mutex<Option<CancellationToken>>,
}

impl AccountState {
    /// A connection to cloud hub `hub_id` as the signed-in person, which
    /// gets a fresh token each time it connects.
    pub fn link(&self, hub_id: &str) -> Option<std::sync::Arc<CloudLink>> {
        let cfg = self.supabase.clone()?;
        let path = self.path.clone();
        let token: TokenSource = std::sync::Arc::new(move || {
            let (cfg, path) = (cfg.clone(), path.clone());
            Box::pin(async move {
                account::access_token(&cfg, &path)
                    .await
                    .map_err(|e| anyhow::anyhow!("{e}"))?
                    .ok_or_else(|| anyhow::anyhow!("not signed in"))
            })
        });
        Some(CloudLink::start(
            cloud_link::sync_url(&self.cloud.api, hub_id),
            hub_id.to_string(),
            token,
        ))
    }

    /// The signed-in person's personal hub, if it's ready.
    pub async fn ready_hub(&self) -> Result<String, CloudFailure> {
        let token = cloud_token(self).await?;
        let status = cloud::hub_status(&self.cloud, &token).await?;
        match status.hub {
            Some(h) if h.state == cloud::pb::HubState::Ready as i32 => Ok(h.hub_id),
            _ => Err(CloudError::new(
                cloud::pb::ErrorCode::NotFound,
                "Your private hub isn't set up yet. Set it up first, then move.",
            )
            .into()),
        }
    }

    pub fn new(config_dir: &std::path::Path) -> Self {
        Self {
            path: account::session_path(config_dir),
            supabase: SupabaseConfig::from_env(),
            cloud: CloudConfig::from_env(),
            pending: Mutex::new(None),
        }
    }
}

#[tauri::command]
pub fn account_status(state: State<'_, AccountState>) -> AccountStatus {
    AccountStatus {
        configured: state.supabase.is_some(),
        account: account::load(&state.path).map(|s| s.account),
    }
}

/// Opens the browser to sign in and waits for it to come back. Starting
/// another sign-in cancels one still waiting.
#[tauri::command]
pub async fn account_sign_in(
    state: State<'_, AccountState>,
    provider: Provider,
) -> Result<AccountView, String> {
    let cfg = state
        .supabase
        .clone()
        .ok_or(account::SignInError::NotConfigured.to_string())?;
    let cancel = CancellationToken::new();
    if let Some(old) = state.pending.lock().unwrap().replace(cancel.clone()) {
        old.cancel();
    }
    let result = account::sign_in(
        &cfg,
        provider,
        |url| {
            if let Err(e) = tauri_plugin_opener::open_url(url, None::<&str>) {
                tracing::warn!(%e, "couldn't open the browser for sign-in");
            }
        },
        cancel.clone(),
    )
    .await;
    // A newer sign-in, or cancelling, cancels ours. Not cancelled means the
    // waiting slot is still ours to clear.
    if !cancel.is_cancelled() {
        state.pending.lock().unwrap().take();
    }
    let session = result.map_err(|e| e.to_string())?;
    account::save(&state.path, &session)
        .map_err(|e| format!("signed in, but couldn't save the session: {e}"))?;
    Ok(session.account)
}

/// Stops a sign-in that's waiting on the browser.
#[tauri::command]
pub fn account_cancel_sign_in(state: State<'_, AccountState>) {
    if let Some(c) = state.pending.lock().unwrap().take() {
        c.cancel();
    }
}

#[tauri::command]
pub async fn account_sign_out(state: State<'_, AccountState>) -> Result<(), String> {
    if let (Some(cfg), Some(session)) = (&state.supabase, account::load(&state.path)) {
        account::revoke(cfg, &session).await;
    }
    account::remove(&state.path).map_err(|e| e.to_string())
}

/// A cloud error for the UI: `code` is a `tab.cloud.v1.ErrorCode` value
/// (the UI has the generated enum), `message` is safe to show.
#[derive(serde::Serialize)]
pub struct CloudFailure {
    code: i32,
    message: String,
}

impl CloudFailure {
    pub fn internal(e: impl std::fmt::Display) -> Self {
        CloudError::new(cloud::pb::ErrorCode::Internal, e.to_string()).into()
    }
}

impl From<CloudError> for CloudFailure {
    fn from(e: CloudError) -> Self {
        Self {
            code: e.code as i32,
            message: e.message,
        }
    }
}

async fn cloud_token(state: &AccountState) -> Result<String, CloudFailure> {
    let cfg = state
        .supabase
        .as_ref()
        .ok_or_else(CloudError::not_signed_in)?;
    match account::access_token(cfg, &state.path).await {
        Ok(Some(t)) => Ok(t),
        Ok(None) => Err(CloudError::not_signed_in().into()),
        Err(account::SignInError::Network(e)) => Err(CloudError::unreachable(e).into()),
        Err(_) => Err(CloudError::not_signed_in().into()),
    }
}

/// The message as Protobuf bytes, which the UI decodes with the types
/// generated from the same account.proto.
fn bytes(m: impl Message) -> Response {
    Response::new(m.encode_to_vec())
}

/// The signed-in person's personal cloud hub: a `tab.cloud.v1.HubStatus`.
#[tauri::command]
pub async fn cloud_hub_status(state: State<'_, AccountState>) -> Result<Response, CloudFailure> {
    let token = cloud_token(&state).await?;
    Ok(bytes(cloud::hub_status(&state.cloud, &token).await?))
}

/// Every cloud hub they belong to: a `tab.cloud.v1.HubList`.
#[tauri::command]
pub async fn cloud_hubs(state: State<'_, AccountState>) -> Result<Response, CloudFailure> {
    let token = cloud_token(&state).await?;
    Ok(bytes(cloud::list_hubs(&state.cloud, &token).await?))
}

/// Sets up their personal hub (or retries a failed setup) and resolves once
/// it's ready or has failed: a `tab.cloud.v1.HubStatus`.
#[tauri::command]
pub async fn cloud_hub_set_up(state: State<'_, AccountState>) -> Result<Response, CloudFailure> {
    let token = cloud_token(&state).await?;
    Ok(bytes(cloud::set_up_hub(&state.cloud, &token).await?))
}
