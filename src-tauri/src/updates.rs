//! Updating tab itself: checks GitHub releases for a newer signed build,
//! downloads and installs it, then restarts the app.
//!
//! The release workflow publishes `latest.json` and the signed update
//! bundles (docs/RELEASING.md). The public key that verifies them comes from
//! `plugins.updater.pubkey` in the config, which release builds fill in from
//! the `TAURI_UPDATER_PUBKEY` variable. A build without one (a dev build, or a
//! release before the key was set up) says so instead of checking.

use std::sync::Mutex;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Runtime, State};
use tauri_plugin_updater::{Update, UpdaterExt};

/// The update found by the last check, kept so installing doesn't fetch the
/// manifest again (and can't install a different version than was shown).
#[derive(Default)]
pub struct PendingUpdate(Mutex<Option<Update>>);

#[derive(Serialize)]
pub struct AppVersion {
    version: String,
    /// Whether this build can update itself.
    updates: bool,
    /// Why not, when it can't.
    reason: Option<String>,
}

#[derive(Serialize)]
pub struct AvailableUpdate {
    version: String,
    notes: Option<String>,
    date: Option<String>,
}

#[derive(Clone, Serialize)]
struct Progress {
    downloaded: u64,
    total: Option<u64>,
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// None when this build can update itself, otherwise the reason it can't.
fn unavailable<R: Runtime>(app: &AppHandle<R>) -> Option<String> {
    if cfg!(debug_assertions) {
        return Some("Development builds don't update themselves.".into());
    }
    let key = app
        .config()
        .plugins
        .0
        .get("updater")
        .and_then(|u| u.get("pubkey"))
        .and_then(|k| k.as_str())
        .unwrap_or("");
    if key.trim().is_empty() {
        return Some(
            "This build wasn't made with an update key, so it can't check for updates.".into(),
        );
    }
    None
}

#[tauri::command]
pub fn app_version<R: Runtime>(app: AppHandle<R>) -> AppVersion {
    let reason = unavailable(&app);
    AppVersion {
        version: app.package_info().version.to_string(),
        updates: reason.is_none(),
        reason,
    }
}

/// Looks for a newer release. None means this is the newest.
#[tauri::command]
pub async fn app_update_check<R: Runtime>(
    app: AppHandle<R>,
    pending: State<'_, PendingUpdate>,
) -> Result<Option<AvailableUpdate>, String> {
    if let Some(reason) = unavailable(&app) {
        return Err(reason);
    }
    let update = app.updater().map_err(err)?.check().await.map_err(err)?;
    let found = update.as_ref().map(|u| AvailableUpdate {
        version: u.version.clone(),
        notes: u.body.clone().filter(|b| !b.trim().is_empty()),
        date: u.date.map(|d| d.to_string()),
    });
    *pending.0.lock().unwrap() = update;
    Ok(found)
}

/// Downloads and installs the update the last check found, then restarts.
/// Progress arrives as `tab://app-update` events.
#[tauri::command]
pub async fn app_update_install<R: Runtime>(
    app: AppHandle<R>,
    pending: State<'_, PendingUpdate>,
) -> Result<(), String> {
    let update = pending
        .0
        .lock()
        .unwrap()
        .take()
        .ok_or("There's no update to install. Check for updates first.")?;
    let mut downloaded = 0u64;
    let events = app.clone();
    let result = update
        .download_and_install(
            move |chunk, total| {
                downloaded += chunk as u64;
                let _ = events.emit("tab://app-update", Progress { downloaded, total });
            },
            || {},
        )
        .await;
    if let Err(e) = result {
        // Keep it, so trying again doesn't need another check.
        *pending.0.lock().unwrap() = Some(update);
        return Err(err(e));
    }
    tracing::info!(version = %update.version, "update installed, restarting");
    app.restart()
}
