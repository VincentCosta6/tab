//! Signing in to a tab account (Supabase, with Apple or Google). The session
//! stays in `account.json` beside `config.json`; the UI only sees who's
//! signed in.

use std::path::PathBuf;
use std::sync::Mutex;

use tab_core::account::{
    self, AccountStatus, AccountView, CancellationToken, Provider, SupabaseConfig,
};
use tab_core::cloud::{self, CloudConfig, CloudError, CloudHub};
use tauri::State;

pub struct AccountState {
    path: PathBuf,
    supabase: Option<SupabaseConfig>,
    cloud: CloudConfig,
    /// The sign-in waiting on the browser, if any.
    pending: Mutex<Option<CancellationToken>>,
}

impl AccountState {
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

/// A cloud error for the UI: `code` to branch on, `message` to show.
#[derive(serde::Serialize)]
pub struct CloudFailure {
    code: &'static str,
    message: String,
}

impl From<CloudError> for CloudFailure {
    fn from(e: CloudError) -> Self {
        Self {
            code: e.code(),
            message: e.to_string(),
        }
    }
}

async fn cloud_token(state: &AccountState) -> Result<String, CloudFailure> {
    let cfg = state.supabase.as_ref().ok_or(CloudError::NotSignedIn)?;
    match account::access_token(cfg, &state.path).await {
        Ok(Some(t)) => Ok(t),
        Ok(None) => Err(CloudError::NotSignedIn.into()),
        Err(account::SignInError::Network(e)) => Err(CloudError::Network(e).into()),
        Err(_) => Err(CloudError::NotSignedIn.into()),
    }
}

/// Where the signed-in person's cloud hub is.
#[tauri::command]
pub async fn cloud_hub_status(state: State<'_, AccountState>) -> Result<CloudHub, CloudFailure> {
    let token = cloud_token(&state).await?;
    Ok(cloud::hub_status(&state.cloud, &token).await?)
}

/// Sets up the signed-in person's private hub (or retries a failed setup)
/// and resolves once it's ready or has failed.
#[tauri::command]
pub async fn cloud_hub_set_up(state: State<'_, AccountState>) -> Result<CloudHub, CloudFailure> {
    let token = cloud_token(&state).await?;
    Ok(cloud::set_up_hub(&state.cloud, &token).await?)
}
