//! The chat's PR bar and autofix: the commands behind the bar, and the loop
//! that looks at each link with autofix on and hands new problems to the
//! task's agent. The rules (schedule, what's new, the message) are
//! `tab_core::autofix`; what a link's problems are is its connector's
//! (`Connector::fix_report`).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use tab_core::autofix::{self, Watch};
use tab_core::model::ContextStatus;
use tauri::State;

use crate::{AppState, CmdResult, Core, err};

/// The bar's state for each link in this task's chat.
#[tauri::command]
pub async fn autofix_list(state: State<'_, AppState>, item_id: String) -> CmdResult<Vec<Watch>> {
    state.store.autofix_for_item(&item_id).map_err(err)
}

/// Turns autofix on or off for one link, and remembers the choice for new
/// PRs ([`adopt`]). On looks right away.
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
    remember(&state, on).await.map_err(err)?;
    if on {
        look_now(&state.core, &item_id, &url).await;
    }
    state.store.autofix_for_item(&item_id).map_err(err)
}

/// Keeps the person's last autofix choice as the default for new PRs.
async fn remember(core: &Core, on: bool) -> anyhow::Result<()> {
    let mut current = core.config.lock().await;
    if current.agent.autofix == on {
        return Ok(());
    }
    let mut next = current.clone();
    next.agent.autofix = on;
    next.save(&core.config_path)?;
    *current = next;
    Ok(())
}

/// Turns autofix on for links the person hasn't chosen for yet, when their
/// last choice was on, and looks at each right away. A link turned off or
/// dismissed keeps its row, so it's left alone.
pub async fn adopt(core: Arc<Core>, statuses: HashMap<String, Vec<ContextStatus>>) {
    if !core.config.lock().await.agent.autofix {
        return;
    }
    for (item_id, list) in statuses {
        for url in list.into_iter().filter(|s| s.autofix).filter_map(|s| s.url) {
            match core.store.autofix_get(&item_id, &url) {
                Ok(None) => {}
                Ok(Some(_)) => continue,
                Err(e) => {
                    tracing::warn!(item = %item_id, %url, error = %e, "autofix: reading a link failed");
                    continue;
                }
            }
            if let Err(e) = core.store.autofix_enable(&item_id, &url, true, Utc::now()) {
                tracing::warn!(item = %item_id, %url, error = %e, "autofix: turning on failed");
                continue;
            }
            look_now(&core, &item_id, &url).await;
        }
    }
}

/// Looks at one link now instead of on the loop's next tick.
async fn look_now(core: &Core, item_id: &str, url: &str) {
    let w = match core.store.autofix_get(item_id, url) {
        Ok(Some(w)) if w.enabled && !w.dismissed => w,
        Ok(_) => return,
        Err(e) => {
            tracing::warn!(item = %item_id, %url, error = %e, "autofix: reading a link failed");
            return;
        }
    };
    if let Err(e) = look(core, &w).await {
        tracing::warn!(item = %item_id, %url, error = %e, "autofix failed");
    }
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
    if let Some(text) = message {
        let sent = core
            .hub
            .session_for_item(&w.item_id)
            .and_then(|s| core.hub.send_with(&s.id, &text, &[]));
        if let Err(e) = sent {
            // Not handed over: try again next time.
            next.handled = w.handled.clone();
            next.last = Some(format!("Couldn't message the agent: {e}"));
        }
    }
    core.store.autofix_checked(&next)
}
