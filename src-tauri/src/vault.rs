//! Commands for a project's end-to-end encrypted vault (Knowledge → Vault).
//! See `tab_core::e2e`.

use std::sync::Arc;

use tab_core::e2e::{AuditView, E2e, Entry, EntryInput, Files, Status};
use tab_core::local_hub::LocalHub;
use tauri::{Manager, State};

use crate::{AppState, CmdResult, err, percent_decode, safe_file_name};

/// Sealed vault files go into the hub's file store, like any file.
pub struct HubFiles(pub Arc<LocalHub>);

#[async_trait::async_trait]
impl Files for HubFiles {
    async fn put(&self, bytes: Vec<u8>) -> anyhow::Result<String> {
        let hub = self.0.clone();
        Ok(tokio::task::spawn_blocking(move || hub.file_put(&bytes))
            .await??
            .hash)
    }

    async fn get(&self, hash: &str) -> anyhow::Result<Option<Vec<u8>>> {
        let (hub, hash) = (self.0.clone(), hash.to_string());
        tokio::task::spawn_blocking(move || hub.file_bytes(&hash)).await?
    }
}

fn vault(state: &AppState) -> CmdResult<&Arc<E2e>> {
    state
        .hub
        .vault()
        .ok_or_else(|| "the vault isn't available on this computer".to_string())
}

#[tauri::command]
pub async fn vault_status(state: State<'_, AppState>) -> CmdResult<Status> {
    vault(&state)?.status().map_err(err)
}

/// Sets the vault up. Returns the recovery key's words, shown once.
#[tauri::command]
pub async fn vault_create(state: State<'_, AppState>) -> CmdResult<String> {
    vault(&state)?.create().await.map_err(err)
}

/// Touch ID / Windows Hello, then open for 10 minutes.
#[tauri::command]
pub async fn vault_unlock(state: State<'_, AppState>) -> CmdResult<()> {
    vault(&state)?.unlock().await.map_err(err)
}

#[tauri::command]
pub async fn vault_lock(state: State<'_, AppState>) -> CmdResult<()> {
    vault(&state)?.lock();
    Ok(())
}

/// Asks an open device to let this one in. Returns the code to compare.
#[tauri::command]
pub async fn vault_ask(state: State<'_, AppState>) -> CmdResult<String> {
    vault(&state)?.ask().map_err(err)
}

#[tauri::command]
pub async fn vault_recover(state: State<'_, AppState>, words: String) -> CmdResult<()> {
    vault(&state)?.recover(&words).await.map_err(err)
}

#[tauri::command]
pub async fn vault_approve(state: State<'_, AppState>, device_id: String) -> CmdResult<()> {
    vault(&state)?.approve(&device_id).map_err(err)
}

#[tauri::command]
pub async fn vault_deny(state: State<'_, AppState>, device_id: String) -> CmdResult<()> {
    vault(&state)?.deny(&device_id).map_err(err)
}

/// Takes a device out; the vault key changes for everyone else.
#[tauri::command]
pub async fn vault_remove_device(state: State<'_, AppState>, device_id: String) -> CmdResult<()> {
    vault(&state)?.remove(&device_id).await.map_err(err)
}

#[tauri::command]
pub async fn vault_entries(
    state: State<'_, AppState>,
    project_id: String,
) -> CmdResult<Vec<Entry>> {
    vault(&state)?.entries(&project_id).map_err(err)
}

#[tauri::command]
pub async fn vault_entry_save(
    state: State<'_, AppState>,
    project_id: String,
    entry: EntryInput,
) -> CmdResult<Entry> {
    vault(&state)?.save(&project_id, entry).await.map_err(err)
}

#[tauri::command]
pub async fn vault_entry_delete(
    state: State<'_, AppState>,
    project_id: String,
    id: String,
) -> CmdResult<()> {
    vault(&state)?.delete(&project_id, &id).await.map_err(err)
}

/// One field's value, for the eye button.
#[tauri::command]
pub async fn vault_reveal(
    state: State<'_, AppState>,
    project_id: String,
    id: String,
    label: String,
) -> CmdResult<String> {
    vault(&state)?.reveal(&project_id, &id, &label).map_err(err)
}

/// Adds a file: sealed on this device before it goes into the hub. The
/// body is the file; the project and name come in headers.
#[tauri::command]
pub async fn vault_file_add(
    state: State<'_, AppState>,
    request: tauri::ipc::Request<'_>,
) -> CmdResult<Entry> {
    let tauri::ipc::InvokeBody::Raw(bytes) = request.body() else {
        return Err("expected the file's bytes".into());
    };
    let header = |k: &str| {
        request
            .headers()
            .get(k)
            .and_then(|v| v.to_str().ok())
            .map(percent_decode)
            .unwrap_or_default()
    };
    let name = safe_file_name(&header("x-name"));
    vault(&state)?
        .add_file(&header("x-project"), &name, bytes)
        .await
        .map_err(err)
}

/// Saves a vault file, decrypted, into Downloads and returns where.
#[tauri::command]
pub async fn vault_file_export(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    project_id: String,
    id: String,
) -> CmdResult<String> {
    let dir = app.path().download_dir().map_err(err)?;
    let path = vault(&state)?
        .export(&project_id, &id, &dir)
        .await
        .map_err(err)?;
    Ok(path.display().to_string())
}

#[tauri::command]
pub async fn vault_audit(
    state: State<'_, AppState>,
    project_id: Option<String>,
) -> CmdResult<Vec<AuditView>> {
    vault(&state)?
        .audit_log(project_id.as_deref(), 200)
        .map_err(err)
}
