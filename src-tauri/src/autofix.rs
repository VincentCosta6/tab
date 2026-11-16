//! The chat's PR bar and autofix: the commands behind the bar, and the loop
//! that looks at each link with autofix on and hands new problems to the
//! task's agent. The rules (schedule, what's new, the message) are
//! `tab_core::autofix`; what a link's problems are is its connector's
//! (`Connector::fix_report`).

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use tab_core::autofix::{self, Watch};
use tauri::State;

use crate::{AppState, CmdResult, Core, err};

/// The bar's state for each link in this task's chat.
#[tauri::command]
pub async fn autofix_list(state: State<'_, AppState>, item_id: String) -> CmdResult<Vec<Watch>> {
    state.store.autofix_for_item(&item_id).map_err(err)
}

/// Turns autofix on or off for one link. On looks right away.
#[tauri::command]
pub async fn autofix_set(
    state: State<'_, AppState>,
    item_id: String,
    url: String,
    on: bool,
) -> CmdResult<Vec<Watch>> {
    state
        .store
        .autofix_enable(&item_id, &url, on, Utc::now())
        .map_err(err)?;
    state.store.autofix_for_item(&item_id).map_err(err)
}

/// Hides the bar for one link (which turns its autofix off), or shows it again.
#[tauri::command]
pub async fn autofix_dismiss(
    state: State<'_, AppState>,
    item_id: String,
    url: String,
    dismissed: bool,
) -> CmdResult<Vec<Watch>> {
    state
        .store
        .autofix_dismiss(&item_id, &url, dismissed, Utc::now())
        .map_err(err)?;
    state.store.autofix_for_item(&item_id).map_err(err)
}

/// Looks for due links every 15 seconds; each link's own schedule
/// (`autofix::next_check`) decides how often it's actually read.
pub fn spawn(core: Arc<Core>) {
    tauri::async_runtime::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(15));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            let due = match core.store.autofix_due(Utc::now()) {
                Ok(d) => d,
                Err(e) => {
                    tracing::warn!(error = %e, "autofix: reading due links failed");
                    continue;
                }
            };
            for w in due {
                if let Err(e) = look(&core, &w).await {
                    tracing::warn!(item = %w.item_id, url = %w.url, error = %e, "autofix failed");
                }
            }
        }
    });
}

async fn look(core: &Core, w: &Watch) -> anyhow::Result<()> {
    let now = Utc::now();
    let session = core.store.session_for_item(&w.item_id)?;
    // The device that runs the chat does the fixing.
    if session.as_ref().is_some_and(|s| !core.hub.runs_here(s)) {
        let mut next = w.clone();
        next.next_at = Some(autofix::next_check(w.active_at, now));
        next.last = Some("This chat runs on another device".into());
        return core.store.autofix_checked(&next);
    }
    let report = {
        let guard = core.engine.read().await;
        match guard.as_ref() {
            Some(e) => e.fix_report(&w.url).await,
            None => return Ok(()),
        }
    };
    let report = match report {
        Ok(r) => r,
        Err(e) => {
            let mut next = w.clone();
            next.checked_at = Some(now);
            next.next_at = Some(autofix::next_check(w.active_at, now));
            next.last = Some(format!("Couldn't check: {e}"));
            return core.store.autofix_checked(&next);
        }
    };
    let idle = session.as_ref().is_none_or(|s| !core.hub.is_running(&s.id));
    let (mut next, message) = autofix::step(w, report.as_ref(), idle, now);
    if let Some(handoff) = message {
        let mut meta = serde_json::Map::new();
        meta.insert("autofix".into(), serde_json::to_value(&handoff.card)?);
        let sent = core
            .hub
            .session_for_item(&w.item_id)
            .and_then(|s| core.hub.send_tagged(&s.id, &handoff.text, meta));
        if let Err(e) = sent {
            // Not handed over: try again next time.
            next.handled = w.handled.clone();
            next.last = Some(format!("Couldn't message the agent: {e}"));
        }
    }
    core.store.autofix_checked(&next)
}
