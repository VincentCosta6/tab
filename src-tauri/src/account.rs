//! Signing in to a tab account (Supabase, with Apple or Google). The session
//! stays in `account.json` beside `config.json`; the UI only sees who's
//! signed in.

use std::path::PathBuf;
use std::sync::Mutex;

use tab_core::account::{
    self, AccountStatus, AccountView, CancellationToken, Provider, SupabaseConfig,
};
use tauri::State;

pub struct AccountState {
    path: PathBuf,
    supabase: Option<SupabaseConfig>,
    /// The sign-in waiting on the browser, if any.
    pending: Mutex<Option<CancellationToken>>,
}

impl AccountState {
    pub fn new(config_dir: &std::path::Path) -> Self {
        Self {
            path: account::session_path(config_dir),
            supabase: SupabaseConfig::from_env(),
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
