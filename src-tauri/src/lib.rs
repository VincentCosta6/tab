//! Desktop shell. Owns the engine, exposes it to the UI as Tauri commands,
//! and forwards engine events to the webview as `tab://event`.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{Duration, Utc};
use serde::Serialize;
use tab_core::agents::{AgentHub, AgentProbe, Decision, McpEndpoint};
use tab_core::connectors::github_review::{self, NewComment, PullReview};
use tab_core::connectors::registry;
use tab_core::gateway::RateLimitState;
use tab_core::mcp::{EngineAccess, McpServer};
use tab_core::model::{
    Approval, ConnectorInfo, ContextEntry, ContextSection, ItemContext, ItemState, NewContext,
    NewTask, Preview, QueueItem, Session, SessionMessage, SettingKind,
};
use tab_core::ratelimit::Pressure;
use tab_core::store::{AuditEntry, AuditQuery, UsageRow};
use tab_core::{Engine, EngineEvent, EngineHandle, Gateway, Store, TabConfig};
use tauri::{Emitter, Manager, State};
use tokio::sync::{Mutex, RwLock, broadcast};

/// Shown in place of stored secrets; sending it back means "keep the old value".
const SECRET_MASK: &str = "••••••••";

type SharedEngine = Arc<RwLock<Option<EngineHandle>>>;

struct AppState {
    store: Arc<Store>,
    gateway: Arc<Gateway>,
    events: broadcast::Sender<EngineEvent>,
    engine: SharedEngine,
    hub: Arc<AgentHub>,
    mcp_url: std::sync::Mutex<Option<String>>,
    config: Mutex<TabConfig>,
    config_path: PathBuf,
}

/// Lets the MCP server reach whichever engine is running now.
struct LiveEngine(SharedEngine);

#[async_trait]
impl EngineAccess for LiveEngine {
    async fn create_task(&self, task: NewTask) -> anyhow::Result<QueueItem> {
        let guard = self.0.read().await;
        guard
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("engine is not running"))?
            .create_task(task)
    }
    async fn preview(&self, item_id: &str) -> anyhow::Result<Preview> {
        let guard = self.0.read().await;
        guard
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("engine is not running"))?
            .preview(item_id)
            .await
    }
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
        // A connection that's off has nothing in the queue.
        match self.store.sources() {
            Ok(sources) => {
                for source in sources {
                    if source != "tab" && cfg.connector(&source).is_none() {
                        match self.store.remove_source(&source) {
                            Ok(n) => tracing::info!(
                                source,
                                removed = n,
                                "cleared items from a connection that's off"
                            ),
                            Err(e) => tracing::warn!(source, error = %e, "couldn't clear items"),
                        }
                    }
                }
            }
            Err(e) => tracing::warn!(error = %e, "couldn't list sources"),
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
async fn item_preview(state: State<'_, AppState>, id: String) -> CmdResult<Preview> {
    with_engine!(state, e => e.preview(&id).await.map_err(err))
}

#[tauri::command]
async fn rate_pressure(state: State<'_, AppState>) -> CmdResult<Vec<Pressure>> {
    Ok(state.gateway.pressure())
}

// --- Context -------------------------------------------------------------

#[tauri::command]
async fn item_context(state: State<'_, AppState>, id: String) -> CmdResult<Option<ItemContext>> {
    state.store.item_context(&id).map_err(err)
}

#[tauri::command]
async fn context_add(
    state: State<'_, AppState>,
    item_id: String,
    entry: NewContext,
) -> CmdResult<ContextEntry> {
    let e = state
        .store
        .context_add(&item_id, &entry, Utc::now())
        .map_err(err)?;
    let _ = state.events.send(EngineEvent::ContextChanged { item_id });
    Ok(e)
}

#[tauri::command]
async fn context_update(
    state: State<'_, AppState>,
    id: String,
    section: Option<ContextSection>,
    pinned: Option<bool>,
) -> CmdResult<Option<ContextEntry>> {
    let e = state
        .store
        .context_update(&id, section, pinned)
        .map_err(err)?;
    if let Some(e) = &e {
        let _ = state.events.send(EngineEvent::ContextChanged {
            item_id: e.item_id.clone(),
        });
    }
    Ok(e)
}

#[tauri::command]
async fn context_remove(state: State<'_, AppState>, id: String) -> CmdResult<()> {
    let entry = state.store.context_get(&id).map_err(err)?;
    state.store.context_remove(&id).map_err(err)?;
    if let Some(e) = entry {
        let _ = state
            .events
            .send(EngineEvent::ContextChanged { item_id: e.item_id });
    }
    Ok(())
}

// --- Tasks and chat ------------------------------------------------------

#[tauri::command]
async fn create_task(state: State<'_, AppState>, task: NewTask) -> CmdResult<QueueItem> {
    with_engine!(state, e => e.create_task(task).map_err(err))
}

#[derive(Serialize)]
struct Chat {
    session: Option<Session>,
    messages: Vec<SessionMessage>,
    approvals: Vec<Approval>,
    running: bool,
}

#[tauri::command]
async fn chat_open(state: State<'_, AppState>, item_id: String) -> CmdResult<Chat> {
    let session = state.store.session_for_item(&item_id).map_err(err)?;
    let (messages, approvals, running) = match &session {
        Some(s) => (
            state.store.messages(&s.id).map_err(err)?,
            state.hub.pending_approvals(&s.id),
            state.hub.is_running(&s.id),
        ),
        None => (Vec::new(), Vec::new(), false),
    };
    Ok(Chat {
        session,
        messages,
        approvals,
        running,
    })
}

#[derive(Serialize)]
struct ChatStarted {
    item: QueueItem,
    session: Session,
}

/// Sends a chat message. Without an item (the "New" chat), the message
/// first becomes a tab task, and the session hangs off that task.
#[tauri::command]
async fn chat_send(
    state: State<'_, AppState>,
    item_id: Option<String>,
    text: String,
) -> CmdResult<ChatStarted> {
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err("message is empty".into());
    }
    if state.hub.config().kind.is_none() {
        return Err("Set up an agent first (Settings → Agent).".into());
    }
    let item = match item_id {
        Some(id) => state
            .store
            .get(&id)
            .map_err(err)?
            .ok_or("that item no longer exists")?,
        None => {
            let first = text.lines().next().unwrap_or_default();
            let title: String = if first.chars().count() > 80 {
                first.chars().take(77).collect::<String>() + "…"
            } else {
                first.to_string()
            };
            with_engine!(state, e => e.create_task(NewTask {
                title,
                body: (text.len() > first.len()).then(|| text.clone()),
                created_by: "user".into(),
                ..Default::default()
            }).map_err(err))?
        }
    };
    let session = state.hub.session_for_item(&item.id).map_err(err)?;
    state.hub.send(&session.id, &text).map_err(err)?;
    Ok(ChatStarted { item, session })
}

#[tauri::command]
async fn chat_stop(state: State<'_, AppState>, session_id: String) -> CmdResult<()> {
    state.hub.stop(&session_id);
    Ok(())
}

#[tauri::command]
async fn approval_resolve(
    state: State<'_, AppState>,
    id: String,
    decision: String,
    message: Option<String>,
) -> CmdResult<bool> {
    let d = match decision.as_str() {
        "allow" => Decision::Allow,
        "always" => Decision::AllowAlways,
        _ => Decision::Deny(message.unwrap_or_else(|| "The person declined.".into())),
    };
    Ok(state.hub.resolve_approval(&id, d))
}

#[tauri::command]
async fn detect_agents(state: State<'_, AppState>) -> CmdResult<Vec<AgentProbe>> {
    Ok(state.hub.detect().await)
}

#[derive(Serialize)]
struct McpInfo {
    url: Option<String>,
    token: String,
}

#[tauri::command]
async fn mcp_info(state: State<'_, AppState>) -> CmdResult<McpInfo> {
    let url = state.mcp_url.lock().unwrap().clone();
    let token = state.config.lock().await.mcp.token.clone();
    Ok(McpInfo { url, token })
}

// --- Agent spend ---------------------------------------------------------

#[tauri::command]
async fn usage_summary(state: State<'_, AppState>) -> CmdResult<tab_core::store::UsageSummary> {
    state.store.usage_summary(chrono::Utc::now()).map_err(err)
}

// --- GitHub CLI ----------------------------------------------------------

#[tauri::command]
async fn gh_status() -> CmdResult<tab_core::gh::GhStatus> {
    Ok(tab_core::gh::status().await)
}

/// Runs `gh auth login --web`. The one-time code arrives as a `tab://gh-code` event.
#[tauri::command]
async fn gh_login(app: tauri::AppHandle) -> CmdResult<tab_core::gh::GhStatus> {
    tab_core::gh::login(move |code| {
        let _ = app.emit("tab://gh-code", &code);
        let _ = tauri_plugin_opener::open_url(&code.url, None::<&str>);
    })
    .await
    .map_err(err)
}

#[tauri::command]
async fn gh_install() -> CmdResult<tab_core::gh::GhStatus> {
    tab_core::gh::install().await.map_err(err)
}

// --- GitHub review -------------------------------------------------------

#[tauri::command]
async fn github_pull(
    state: State<'_, AppState>,
    repo: String,
    number: u64,
    sample: bool,
) -> CmdResult<PullReview> {
    github_review::pull(&state.gateway, &repo, number, sample)
        .await
        .map_err(err)
}

#[tauri::command]
async fn github_file(
    state: State<'_, AppState>,
    repo: String,
    sha: String,
    path: String,
) -> CmdResult<String> {
    github_review::file_at(&state.gateway, &repo, &sha, &path)
        .await
        .map_err(err)
}

#[allow(clippy::too_many_arguments)]
#[tauri::command]
async fn github_comment(
    state: State<'_, AppState>,
    repo: String,
    number: u64,
    commit_id: String,
    path: String,
    line: u32,
    side: String,
    body: String,
) -> CmdResult<()> {
    github_review::comment(
        &state.gateway,
        NewComment {
            repo: &repo,
            number,
            commit_id: &commit_id,
            path: &path,
            line,
            side: &side,
            body: &body,
        },
    )
    .await
    .map_err(err)
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
    state.hub.set_config(next.agent.clone()).await;
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

            let hub = AgentHub::new(
                store.clone(),
                events.clone(),
                config.agent.clone(),
                data_dir.clone(),
            );
            let engine: SharedEngine = Arc::new(RwLock::new(None));
            let state = AppState {
                store: store.clone(),
                gateway,
                events: events.clone(),
                engine: engine.clone(),
                hub: hub.clone(),
                mcp_url: std::sync::Mutex::new(None),
                config: Mutex::new(config.clone()),
                config_path,
            };
            tauri::async_runtime::block_on(async {
                state.restart(&config).await;
                // Local MCP server for agents.
                let server = Arc::new(McpServer {
                    store,
                    hub: hub.clone(),
                    engine: Arc::new(LiveEngine(engine)),
                    events,
                    token: config.mcp.token.clone(),
                });
                match tab_core::mcp::serve(server, config.mcp.port).await {
                    Ok(url) => {
                        tracing::info!(%url, "MCP server listening");
                        hub.set_mcp(McpEndpoint {
                            url: url.clone(),
                            token: config.mcp.token.clone(),
                        });
                        *state.mcp_url.lock().unwrap() = Some(url);
                    }
                    Err(e) => tracing::error!(error = %e, "MCP server failed to start"),
                }
            });
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
            rate_pressure,
            item_context,
            context_add,
            context_update,
            context_remove,
            create_task,
            chat_open,
            chat_send,
            chat_stop,
            approval_resolve,
            detect_agents,
            mcp_info,
            gh_status,
            usage_summary,
            gh_login,
            gh_install,
            github_pull,
            github_file,
            github_comment,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event
                && let Some(state) = app.try_state::<AppState>()
            {
                // Stops agent processes and the OpenCode server tab started.
                state.hub.shutdown();
            }
        });
}
