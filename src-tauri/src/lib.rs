//! Desktop shell. Owns the engine, exposes it to the UI as Tauri commands,
//! and forwards engine events to the webview as `tab://event`.

use std::path::PathBuf;
use std::sync::Arc;

use chrono::{Duration, Utc};
use serde::Serialize;
use tab_core::connectors::registry;
use tab_core::gateway::RateLimitState;
use tab_core::model::{Block, ConnectorInfo, ItemState, QueueItem, SettingKind};
use tab_core::store::{AuditEntry, AuditQuery, UsageRow};
use tab_core::{Engine, EngineEvent, EngineHandle, Gateway, Store, TabConfig};
use tauri::{Emitter, Manager, State};
use tokio::sync::{Mutex, RwLock, broadcast};

/// Shown in place of stored secrets; sending it back means "keep the old value".
const SECRET_MASK: &str = "••••••••";

struct AppState {
    store: Arc<Store>,
    gateway: Arc<Gateway>,
    events: broadcast::Sender<EngineEvent>,
    engine: RwLock<Option<EngineHandle>>,
    config: Mutex<TabConfig>,
    config_path: PathBuf,
}

type CmdResult<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

impl AppState {
    async fn restart(&self, cfg: &TabConfig) {
        let mut engine = self.engine.write().await;
        if let Some(old) = engine.take() {
            old.shutdown();
        }
        *engine = Some(
            Engine::from_config(
                cfg,
                self.store.clone(),
                self.gateway.clone(),
                self.events.clone(),
            )
            .start(),
        );
    }
}

macro_rules! with_engine {
    ($state:expr, $e:ident => $body:expr) => {{
        let guard = $state.engine.read().await;
        let $e = guard.as_ref().ok_or("engine is not running")?;
        $body
    }};
}

#[derive(Serialize)]
struct ItemDetail {
    item: QueueItem,
    related: Vec<QueueItem>,
}

#[tauri::command]
async fn list_items(
    state: State<'_, AppState>,
    states: Vec<ItemState>,
) -> CmdResult<Vec<QueueItem>> {
    state.store.list(&states).map_err(err)
}

#[tauri::command]
async fn search_items(
    state: State<'_, AppState>,
    query: String,
    states: Vec<ItemState>,
) -> CmdResult<Vec<QueueItem>> {
    state.store.search(&query, &states, 200).map_err(err)
}

#[tauri::command]
async fn item_detail(state: State<'_, AppState>, id: String) -> CmdResult<Option<ItemDetail>> {
    let Some(item) = state.store.get(&id).map_err(err)? else {
        return Ok(None);
    };
    let related = state.store.related_items(&id, 8).map_err(err)?;
    Ok(Some(ItemDetail { item, related }))
}

#[tauri::command]
async fn item_preview(state: State<'_, AppState>, id: String) -> CmdResult<Vec<Block>> {
    with_engine!(state, e => e.preview(&id).await.map_err(err))
}

#[tauri::command]
async fn mark_done(state: State<'_, AppState>, id: String) -> CmdResult<Option<QueueItem>> {
    with_engine!(state, e => e.mark_done(&id).await.map_err(err))
}

#[tauri::command]
async fn snooze(
    state: State<'_, AppState>,
    id: String,
    minutes: i64,
) -> CmdResult<Option<QueueItem>> {
    with_engine!(state, e => e.snooze(&id, Utc::now() + Duration::minutes(minutes.max(1))).map_err(err))
}

#[tauri::command]
async fn reopen(state: State<'_, AppState>, id: String) -> CmdResult<Option<QueueItem>> {
    with_engine!(state, e => e.reopen(&id).map_err(err))
}

#[tauri::command]
async fn list_connectors(state: State<'_, AppState>) -> CmdResult<Vec<ConnectorInfo>> {
    with_engine!(state, e => Ok(e.connectors()))
}

#[tauri::command]
async fn rate_limits(state: State<'_, AppState>) -> CmdResult<Vec<RateLimitState>> {
    Ok(state.gateway.rate_limits())
}

#[tauri::command]
async fn audit_log(state: State<'_, AppState>, query: AuditQuery) -> CmdResult<Vec<AuditEntry>> {
    state.store.audit_list(&query).map_err(err)
}

#[tauri::command]
async fn api_usage(state: State<'_, AppState>, hours: i64) -> CmdResult<Vec<UsageRow>> {
    state
        .store
        .usage_since(Utc::now() - Duration::hours(hours.clamp(1, 24 * 30)))
        .map_err(err)
}

/// Keys of settings that hold secrets, per connector, from the descriptors.
fn secret_keys() -> Vec<(String, String)> {
    registry()
        .into_iter()
        .flat_map(|f| {
            let d = f.descriptor();
            d.settings
                .into_iter()
                .filter(|s| s.kind == SettingKind::Secret)
                .map(move |s| (d.id.clone(), s.key))
        })
        .collect()
}

fn masked(cfg: &TabConfig) -> TabConfig {
    let mut cfg = cfg.clone();
    if cfg.ranking.ai.api_key.is_some() {
        cfg.ranking.ai.api_key = Some(SECRET_MASK.into());
    }
    for (id, key) in secret_keys() {
        if let Some(v) = cfg
            .connectors
            .get_mut(&id)
            .and_then(|c| c.settings.get_mut(&key))
            && v.as_str().is_some_and(|s| !s.is_empty())
        {
            *v = SECRET_MASK.into();
        }
    }
    cfg
}

fn unmask(mut new: TabConfig, old: &TabConfig) -> TabConfig {
    if new.ranking.ai.api_key.as_deref() == Some(SECRET_MASK) {
        new.ranking.ai.api_key = old.ranking.ai.api_key.clone();
    }
    for (id, key) in secret_keys() {
        let keep = new
            .connectors
            .get(&id)
            .and_then(|c| c.settings.get(&key))
            .and_then(|v| v.as_str())
            == Some(SECRET_MASK);
        if keep {
            let prev = old
                .connectors
                .get(&id)
                .and_then(|c| c.settings.get(&key))
                .cloned()
                .unwrap_or_default();
            new.connectors
                .get_mut(&id)
                .unwrap()
                .settings
                .insert(key, prev);
        }
    }
    new
}

#[tauri::command]
async fn get_config(state: State<'_, AppState>) -> CmdResult<TabConfig> {
    Ok(masked(&*state.config.lock().await))
}

#[tauri::command]
async fn save_config(state: State<'_, AppState>, config: TabConfig) -> CmdResult<TabConfig> {
    let mut current = state.config.lock().await;
    let next = unmask(config, &current);
    next.save(&state.config_path).map_err(err)?;
    let ranking_changed = next.ranking != current.ranking || next.profile != current.profile;
    *current = next.clone();
    drop(current);
    state.restart(&next).await;
    if ranking_changed {
        with_engine!(state, e => e.rerank_all().map_err(err))?;
    }
    Ok(masked(&next))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,tab_core=debug".into()),
        )
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            let config_path = app.path().app_config_dir()?.join("config.json");
            let config = TabConfig::load(&config_path)?;
            let store = Arc::new(Store::open(&data_dir.join("tab.db"))?);
            let gateway = Arc::new(Gateway::new(store.clone()));
            let (events, _) = broadcast::channel(1024);

            // Forward engine events to the UI.
            let mut rx = events.subscribe();
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    match rx.recv().await {
                        Ok(ev) => {
                            let _ = handle.emit("tab://event", ev);
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!(n, "UI fell behind on events")
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            });

            let state = AppState {
                store,
                gateway,
                events,
                engine: RwLock::new(None),
                config: Mutex::new(config.clone()),
                config_path,
            };
            tauri::async_runtime::block_on(state.restart(&config));
            app.manage(state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_items,
            search_items,
            item_detail,
            item_preview,
            mark_done,
            snooze,
            reopen,
            list_connectors,
            rate_limits,
            audit_log,
            api_usage,
            get_config,
            save_config,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
