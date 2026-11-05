//! Desktop shell. Owns the engine, exposes it to the UI as Tauri commands,
//! and forwards engine events to the webview as `tab://event`.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use tab_core::agents::{AgentHub, AgentProbe, Decision, McpEndpoint};
use tab_core::config::ConnectorConfig;
use tab_core::connectors::github_review::{self, PrRef, PullReview, ReviewAction};
use tab_core::connectors::registry;
use tab_core::gateway::RateLimitState;
use tab_core::lsp::{self, LspHub, ServerStatus};
use tab_core::mcp::{EngineAccess, McpServer};
use tab_core::model::{
    Approval, ConnectorInfo, ContextEntry, ContextSection, ItemContext, ItemState, NewContext,
    NewTask, Preview, QueueItem, Session, SessionMessage, SettingKind,
};
use tab_core::ratelimit::Pressure;
use tab_core::store::{AuditEntry, AuditQuery, UsageRow};
use tab_core::workspace::{self, SearchHit, SearchQuery, Target, Workspace, Workspaces};
use tab_core::{Engine, EngineEvent, EngineHandle, Gateway, Store, TabConfig};
use tauri::{Emitter, Manager, State};
use tokio::sync::{Mutex, RwLock, broadcast};

/// Shown in place of stored secrets; sending it back means "keep the old value".
const SECRET_MASK: &str = "••••••••";

type SharedEngine = Arc<RwLock<Option<EngineHandle>>>;

/// What the app and the MCP server share: the store, the running engine and settings.
struct Core {
    store: Arc<Store>,
    gateway: Arc<Gateway>,
    events: broadcast::Sender<EngineEvent>,
    engine: SharedEngine,
    hub: Arc<AgentHub>,
    config: Mutex<TabConfig>,
    config_path: PathBuf,
    /// Which embedding model search by meaning wants (None when it's off).
    embed: tokio::sync::watch::Sender<Option<&'static str>>,
}

struct AppState {
    core: Arc<Core>,
    mcp_url: std::sync::Mutex<Option<String>>,
    workspaces: Arc<Workspaces>,
    lsp: Arc<LspHub>,
}

impl std::ops::Deref for AppState {
    type Target = Core;
    fn deref(&self) -> &Core {
        &self.core
    }
}

/// Lets the MCP server reach whichever engine is running now.
struct LiveEngine(Arc<Core>);

#[async_trait]
impl EngineAccess for LiveEngine {
    async fn create_task(&self, task: NewTask) -> anyhow::Result<QueueItem> {
        let guard = self.0.engine.read().await;
        guard
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("engine is not running"))?
            .create_task(task)
    }
    async fn preview(&self, item_id: &str) -> anyhow::Result<Preview> {
        let guard = self.0.engine.read().await;
        guard
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("engine is not running"))?
            .preview(item_id)
            .await
    }
    async fn connectors(&self) -> anyhow::Result<Vec<ConnectorInfo>> {
        let guard = self.0.engine.read().await;
        Ok(guard
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("engine is not running"))?
            .connectors())
    }
    async fn connector_config(&self, id: &str) -> anyhow::Result<Option<ConnectorConfig>> {
        Ok(self.0.config.lock().await.connectors.get(id).cloned())
    }
    async fn save_connector(&self, id: &str, cfg: ConnectorConfig) -> anyhow::Result<()> {
        let mut next = self.0.config.lock().await.clone();
        next.connectors.insert(id.to_string(), cfg);
        self.0.apply(next).await.map(|_| ())
    }
}

type CmdResult<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

impl Core {
    async fn restart(&self, cfg: &TabConfig) {
        self.embed.send_if_modified(|m| {
            let want = tab_core::embed::choice(cfg);
            std::mem::replace(m, want) != want
        });
        let mut engine = self.engine.write().await;
        if let Some(old) = engine.take() {
            old.shutdown();
        }
        // A connection that's off has nothing in the queue.
        match self.store.sources() {
            Ok(sources) => {
                for source in sources {
                    if source != "tab"
                        && (cfg.connector(&source).is_none()
                            || !tab_core::connectors::known(&source))
                    {
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

    /// Saves settings, restarts the engine with them and tells the UI.
    async fn apply(&self, next: TabConfig) -> anyhow::Result<TabConfig> {
        let mut current = self.config.lock().await;
        next.save(&self.config_path)?;
        let ranking_changed = next.ranking != current.ranking || next.profile != current.profile;
        *current = next.clone();
        drop(current);
        self.hub.set_config(next.agent.clone()).await;
        self.restart(&next).await;
        if ranking_changed && let Some(e) = self.engine.read().await.as_ref() {
            e.rerank_all()?;
        }
        let _ = self.events.send(EngineEvent::ConfigChanged);
        Ok(next)
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
    // Keyword matches first; then things that mean the same, once the
    // embedding model is ready.
    let qv = state.store.embed_query(&query);
    state
        .store
        .search_hybrid(&query, qv.as_deref(), &states, &Default::default(), 200)
        .map_err(err)
}

#[tauri::command]
async fn item_detail(state: State<'_, AppState>, id: String) -> CmdResult<Option<ItemDetail>> {
    let Some(item) = state.store.get(&id).map_err(err)? else {
        return Ok(None);
    };
    let related = state.store.related_items(&id, 8).map_err(err)?;
    Ok(Some(ItemDetail { item, related }))
}

/// The item a link points at, so links open in tab before the browser.
#[tauri::command]
async fn item_by_url(state: State<'_, AppState>, url: String) -> CmdResult<Option<QueueItem>> {
    state.store.item_by_url(&url).map_err(err)
}

/// Saves an image the person pasted or dropped into a chat. The body is the
/// raw bytes; the `x-name` header carries the original file name.
#[tauri::command]
async fn attachment_save(
    state: State<'_, AppState>,
    request: tauri::ipc::Request<'_>,
) -> CmdResult<String> {
    let tauri::ipc::InvokeBody::Raw(bytes) = request.body() else {
        return Err("expected the image's bytes".into());
    };
    if bytes.len() > MAX_IMAGE {
        return Err("that image is over 20 MB".into());
    }
    let name = request
        .headers()
        .get("x-name")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("image.png");
    let ext = image_ext(name).ok_or("tab takes PNG, JPEG, GIF and WebP images")?;
    let dir = state.hub.attachments_dir();
    std::fs::create_dir_all(&dir).map_err(err)?;
    let path = dir.join(format!("{}.{ext}", uuid::Uuid::new_v4()));
    std::fs::write(&path, bytes).map_err(err)?;
    Ok(path.display().to_string())
}

const MAX_IMAGE: usize = 20 * 1024 * 1024;

fn image_ext(name: &str) -> Option<&'static str> {
    let ext = name.rsplit('.').next()?.to_ascii_lowercase();
    ["png", "jpg", "jpeg", "gif", "webp"]
        .into_iter()
        .find(|e| *e == ext)
}

/// An image file's bytes, for thumbnails and the viewer: attachments, and
/// screenshots an agent mentions by path. Only image files are served.
#[tauri::command]
async fn image_read(path: String) -> CmdResult<tauri::ipc::Response> {
    image_ext(&path).ok_or("not an image")?;
    let meta = std::fs::metadata(&path).map_err(err)?;
    if !meta.is_file() || meta.len() as usize > MAX_IMAGE {
        return Err("not an image tab can show".into());
    }
    Ok(tauri::ipc::Response::new(
        std::fs::read(&path).map_err(err)?,
    ))
}

/// Starts the system's own dictation in the focused text box (macOS: the
/// same as Edit → Start Dictation). Elsewhere, says how to start it.
#[tauri::command]
fn start_dictation(app: tauri::AppHandle) -> CmdResult<()> {
    #[cfg(target_os = "macos")]
    {
        app.run_on_main_thread(|| {
            use objc2::runtime::{AnyClass, AnyObject};
            use objc2::{msg_send, sel};
            let Some(class) = AnyClass::get(c"NSApplication") else {
                return;
            };
            let none: *mut AnyObject = std::ptr::null_mut();
            // SAFETY: plain AppKit messages on the main thread; a nil target
            // sends the action to the first responder (the focused field).
            unsafe {
                let ns_app: *mut AnyObject = msg_send![class, sharedApplication];
                let _: bool =
                    msg_send![ns_app, sendAction: sel!(startDictation:), to: none, from: none];
            }
        })
        .map_err(err)?;
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
        Err(if cfg!(windows) {
            "Press Windows + H to dictate.".into()
        } else {
            "Use your desktop's dictation to talk into the box.".into()
        })
    }
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
    adopt_project(&state, &e);
    let _ = state.events.send(EngineEvent::ContextChanged { item_id });
    Ok(e)
}

/// Something added to a chat's Context joins the chat's project, unless it
/// already has one.
fn adopt_project(state: &AppState, e: &ContextEntry) {
    let Some(r) = e
        .ref_item
        .as_ref()
        .filter(|_| e.section == ContextSection::Context)
    else {
        return;
    };
    if let Ok(Some(item)) = state.store.adopt_project(&e.item_id, &r.id, Utc::now()) {
        let _ = state.events.send(EngineEvent::ItemChanged {
            change: tab_core::engine::Change::Updated,
            item,
        });
    }
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
        adopt_project(&state, e);
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
    project_id: Option<String>,
    attachments: Option<Vec<String>>,
) -> CmdResult<ChatStarted> {
    let attachments = attachments.unwrap_or_default();
    let mut text = text.trim().to_string();
    if text.is_empty() && attachments.is_empty() {
        return Err("message is empty".into());
    }
    if text.is_empty() {
        text = "Have a look at this.".into();
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
                project_id,
                ..Default::default()
            }).map_err(err))?
        }
    };
    let session = state.hub.session_for_item(&item.id).map_err(err)?;
    state
        .hub
        .send_with(&session.id, &text, &attachments)
        .map_err(err)?;
    Ok(ChatStarted { item, session })
}

/// The PR a review chat is about.
#[derive(Deserialize)]
struct ReviewChatTarget {
    repo: String,
    number: u64,
    title: String,
    url: String,
    head_ref: String,
    base_ref: String,
}

fn review_chat_key(repo: &str, number: u64) -> String {
    format!("review:{repo}#{number}")
}

/// The agent chat for a pull request under review.
#[tauri::command]
async fn review_chat_open(
    state: State<'_, AppState>,
    repo: String,
    number: u64,
) -> CmdResult<Chat> {
    let session = state
        .store
        .session_for_key(&review_chat_key(&repo, number))
        .map_err(err)?;
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

/// Sends a message to the PR's agent chat. The agent runs in the PR's local
/// checkout when there is one.
#[tauri::command]
async fn review_chat_send(
    state: State<'_, AppState>,
    target: ReviewChatTarget,
    text: String,
    attachments: Option<Vec<String>>,
) -> CmdResult<Session> {
    let attachments = attachments.unwrap_or_default();
    let mut text = text.trim().to_string();
    if text.is_empty() && attachments.is_empty() {
        return Err("message is empty".into());
    }
    if text.is_empty() {
        text = "Have a look at this.".into();
    }
    if state.hub.config().kind.is_none() {
        return Err("Set up an agent first (Settings → Agent).".into());
    }
    let dir = state
        .workspaces
        .worktree(&target.repo, target.number)
        .map_err(err)?;
    let local = dir.exists();
    let cwd = if local {
        dir.display().to_string()
    } else {
        state.hub.workdir()
    };
    let brief = review_brief(&target, local.then_some(cwd.as_str()));
    let session = state
        .hub
        .session_for_key(
            &review_chat_key(&target.repo, target.number),
            &format!("{}#{}: {}", target.repo, target.number, target.title),
            &cwd,
            &brief,
        )
        .map_err(err)?;
    state
        .hub
        .send_with(&session.id, &text, &attachments)
        .map_err(err)?;
    Ok(session)
}

fn review_brief(t: &ReviewChatTarget, checkout: Option<&str>) -> String {
    let mut b = format!(
        "This session is about pull request {}#{} \"{}\" ({}), merging {} into {}. \
         The person is reviewing it in tab's review workspace and talks to you from a side panel.\n",
        t.repo, t.number, t.title, t.url, t.head_ref, t.base_ref
    );
    match checkout {
        Some(dir) => b.push_str(&format!(
            "The PR's head is checked out at {dir} (your working directory), with the base branch \
             fetched, so `git diff origin/{}...HEAD` shows the PR's changes. Read code there instead of guessing.\n",
            t.base_ref
        )),
        None => b.push_str(
            "There is no local checkout of this PR; use `gh pr view` / `gh pr diff` or the GitHub API to read it.\n",
        ),
    }
    b.push_str(
        "- Link every piece of code you mention as a markdown link to its path from the repository root \
         with a line anchor, like [refund()](src/refund.ts#L12) or [the retry loop](src/capture.ts#L8-L20). \
         tab opens these in its editor, so prefer linking definitions over pasting them.\n\
         - When asked what code does, explain briefly and link the definitions and call sites that matter.\n\
         - When given a fix list, make each fix in the checkout, keeping changes minimal, then reply with \
         one line per fix saying what you changed (linked) or why you didn't. Don't commit or push unless \
         the person asks; they review your changes first.\n",
    );
    b
}

// --- Projects ------------------------------------------------------------

#[tauri::command]
async fn projects_list(state: State<'_, AppState>) -> CmdResult<Vec<tab_core::model::Project>> {
    state.store.projects().map_err(err)
}

#[tauri::command]
async fn project_save(
    state: State<'_, AppState>,
    id: Option<String>,
    name: String,
    color: String,
    icon: String,
) -> CmdResult<tab_core::model::Project> {
    state
        .store
        .project_save(id.as_deref(), &name, &color, &icon, Utc::now())
        .map_err(err)
}

#[tauri::command]
async fn project_delete(state: State<'_, AppState>, id: String) -> CmdResult<()> {
    state.store.project_delete(&id).map_err(err)
}

#[tauri::command]
async fn project_notes(
    state: State<'_, AppState>,
    project_id: String,
) -> CmdResult<Vec<tab_core::model::ProjectNote>> {
    state.store.project_notes(&project_id).map_err(err)
}

/// The person adding a note (no id) or rewriting one.
#[tauri::command]
async fn project_note_save(
    state: State<'_, AppState>,
    project_id: String,
    id: Option<String>,
    topic: String,
    text: String,
) -> CmdResult<()> {
    match id {
        Some(id) => {
            state.store.note_edit(&id, &text, Utc::now()).map_err(err)?;
        }
        None => {
            state
                .store
                .note_save(&project_id, &topic, &text, "user", None, &[], Utc::now())
                .map_err(err)?;
        }
    }
    Ok(())
}

#[tauri::command]
async fn project_note_delete(state: State<'_, AppState>, id: String) -> CmdResult<()> {
    state.store.note_delete(&id).map_err(err)?;
    Ok(())
}

#[tauri::command]
async fn item_set_project(
    state: State<'_, AppState>,
    id: String,
    project_id: Option<String>,
) -> CmdResult<Option<QueueItem>> {
    let item = state
        .store
        .item_set_project(&id, project_id.as_deref(), Utc::now())
        .map_err(err)?;
    if let Some(i) = &item {
        let _ = state.events.send(EngineEvent::ItemChanged {
            change: tab_core::engine::Change::Updated,
            item: i.clone(),
        });
    }
    Ok(item)
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
    answers: Option<serde_json::Value>,
) -> CmdResult<bool> {
    let d = match decision.as_str() {
        "allow" => Decision::Allow,
        "answer" => Decision::Answer(answers.unwrap_or_default()),
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

/// A slice of the knowledge graph to draw, optionally one project's and
/// optionally only recent activity.
#[tauri::command]
async fn graph_view(
    state: State<'_, AppState>,
    project_id: Option<String>,
    days: Option<i64>,
    limit: Option<usize>,
) -> CmdResult<serde_json::Value> {
    let since = days.map(|d| Utc::now() - chrono::Duration::days(d.max(1)));
    let (nodes, edges) = state
        .store
        .graph_view(
            project_id.as_deref(),
            since,
            limit.unwrap_or(150).clamp(10, 500),
        )
        .map_err(err)?;
    Ok(serde_json::json!({
        "nodes": nodes,
        "edges": edges.iter().map(|(a, b, w)| serde_json::json!({ "src": a, "dst": b, "weight": w })).collect::<Vec<_>>(),
    }))
}

/// One node's neighbours and recent items, for the side panel.
#[tauri::command]
async fn graph_node(state: State<'_, AppState>, id: i64) -> CmdResult<serde_json::Value> {
    let entity = state.store.entity(id).map_err(err)?.ok_or("no such node")?;
    let connected = state.store.co_entities(id, None, None, 12).map_err(err)?;
    let items = state.store.entity_items(id, None, 12).map_err(err)?;
    let notes = match entity
        .key
        .strip_prefix("project:")
        .filter(|_| entity.kind == "project")
    {
        Some(p) => state.store.project_notes(p).map_err(err)?,
        None => Vec::new(),
    };
    Ok(serde_json::json!({
        "entity": entity,
        "connected": connected.iter().map(|(e, n)| serde_json::json!({ "entity": e, "shared": n })).collect::<Vec<_>>(),
        "items": items,
        "notes": notes,
    }))
}

fn ask_chat_key(project_id: Option<&str>) -> String {
    format!("ask:{}", project_id.unwrap_or("all"))
}

#[tauri::command]
async fn ask_chat_open(state: State<'_, AppState>, project_id: Option<String>) -> CmdResult<Chat> {
    let session = state
        .store
        .session_for_key(&ask_chat_key(project_id.as_deref()))
        .map_err(err)?;
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

/// A question from the Ask page: one ongoing chat per project (or for
/// everything), about what tab knows, with the nodes the person selected.
#[tauri::command]
async fn ask_chat_send(
    state: State<'_, AppState>,
    project_id: Option<String>,
    text: String,
    focus: Option<Vec<String>>,
    attachments: Option<Vec<String>>,
) -> CmdResult<Session> {
    let attachments = attachments.unwrap_or_default();
    let mut text = text.trim().to_string();
    if text.is_empty() && attachments.is_empty() {
        return Err("message is empty".into());
    }
    if text.is_empty() {
        text = "Have a look at this.".into();
    }
    if state.hub.config().kind.is_none() {
        return Err("Set up an agent first (Settings → Agent).".into());
    }
    let project = match &project_id {
        Some(p) => state.store.project_find(p).map_err(err)?,
        None => None,
    };
    if let Some(f) = focus.filter(|f| !f.is_empty()) {
        text = format!("(Selected in the graph: {})\n\n{text}", f.join(", "));
    }
    let brief = format!(
        "This chat is on tab's Ask page: the person is looking at their knowledge graph (projects, repos, \
         people, PRs, tickets, email contacts and how they connect) and asking about their work. Answer \
         from what tab knows: tab_graph_search and tab_graph_expand for the graph, tab_search for items \
         (with a time window when they mean recent work), tab_project for a project's notes and recent \
         work. Name the items you rely on. {}",
        match &project {
            Some(p) => format!(
                "They have the graph filtered to the project \"{}\"; scope to it unless they ask wider (tab_search with project \"{}\").",
                p.name, p.name
            ),
            None => "They're looking at everything, across projects.".to_string(),
        }
    );
    let title = match &project {
        Some(p) => format!("Ask · {}", p.name),
        None => "Ask".to_string(),
    };
    let cwd = state.hub.workdir();
    let session = state
        .hub
        .session_for_key(&ask_chat_key(project_id.as_deref()), &title, &cwd, &brief)
        .map_err(err)?;
    state
        .hub
        .send_with(&session.id, &text, &attachments)
        .map_err(err)?;
    Ok(session)
}

/// Items whose agent is busy, waiting on the person, or failed, for the
/// queue rows and the nav.
#[tauri::command]
async fn agent_activity(state: State<'_, AppState>) -> CmdResult<Vec<serde_json::Value>> {
    let sessions = state.store.sessions_recent(300).map_err(err)?;
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for s in sessions {
        let Some(item) = s.item_id.clone() else {
            continue;
        };
        // Newest session per item only.
        if !seen.insert(item.clone()) {
            continue;
        }
        let waiting = !state.hub.pending_approvals(&s.id).is_empty();
        let status = if waiting {
            "needs_input".to_string()
        } else {
            serde_json::to_value(s.status)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default()
        };
        if status == "idle" {
            continue;
        }
        out.push(serde_json::json!({ "item_id": item, "agent": s.agent, "status": status, "updated_at": s.updated_at }));
    }
    Ok(out)
}

/// Search by meaning: off, downloading, indexing (with progress) or ready.
#[tauri::command]
async fn search_index_status(
    state: State<'_, AppState>,
) -> CmdResult<tab_core::embed::IndexStatus> {
    Ok(state.store.index_status())
}

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

/// Title, state, author and CI for a github.com PR, issue or commit link.
#[tauri::command]
async fn github_link_summary(
    state: State<'_, AppState>,
    repo: String,
    what: String,
    id: String,
) -> CmdResult<github_review::LinkSummary> {
    github_review::link_summary(&state.gateway, &repo, &what, &id)
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

#[tauri::command]
async fn github_review_action(
    state: State<'_, AppState>,
    pr: PrRef,
    action: ReviewAction,
) -> CmdResult<()> {
    github_review::act(&state.gateway, &pr, action)
        .await
        .map_err(err)
}

// --- Review workspace (local checkout) ------------------------------------

impl AppState {
    /// The token git uses to clone: the GitHub connection's, else gh's.
    async fn github_token(&self) -> Option<String> {
        let from_config = self
            .config
            .lock()
            .await
            .connectors
            .get("github")
            .and_then(|c| c.str("token").map(str::to_string));
        match from_config {
            Some(t) => Some(t),
            None => tauri::async_runtime::spawn_blocking(tab_core::gh::token)
                .await
                .ok()
                .flatten(),
        }
    }
}

/// Clones the repo and checks out the PR. Progress arrives as `tab://workspace` events.
#[tauri::command]
async fn review_workspace_prepare(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    target: Target,
) -> CmdResult<Workspace> {
    let token = state.github_token().await;
    state
        .workspaces
        .prepare(&target, token, move |p| {
            let _ = app.emit("tab://workspace", &p);
        })
        .await
        .map_err(err)
}

#[tauri::command]
async fn review_workspace_file(
    state: State<'_, AppState>,
    repo: String,
    sha: String,
    path: String,
) -> CmdResult<String> {
    state.workspaces.read(&repo, &sha, &path).await.map_err(err)
}

#[tauri::command]
async fn review_workspace_files(
    state: State<'_, AppState>,
    repo: String,
    number: u64,
    paths: Vec<String>,
) -> CmdResult<Vec<(String, String)>> {
    state
        .workspaces
        .read_many(&repo, number, &paths)
        .await
        .map_err(err)
}

#[tauri::command]
async fn review_workspace_search(
    state: State<'_, AppState>,
    repo: String,
    number: u64,
    query: SearchQuery,
) -> CmdResult<Vec<SearchHit>> {
    state
        .workspaces
        .search(&repo, number, &query)
        .await
        .map_err(err)
}

/// Opens the PR's checkout in VS Code (`vscode`) or the file manager (`folder`).
#[tauri::command]
async fn review_workspace_open(
    state: State<'_, AppState>,
    repo: String,
    number: u64,
    target: String,
    path: Option<String>,
    line: Option<u32>,
) -> CmdResult<()> {
    let dir = state.workspaces.worktree(&repo, number).map_err(err)?;
    if !dir.exists() {
        return Err("This pull request hasn't been checked out on this computer yet.".into());
    }
    match target.as_str() {
        "vscode" => {
            workspace::open_in_vscode(&dir, path.as_deref().map(|p| (p, line.unwrap_or(1))))
                .map_err(err)
        }
        _ => tauri_plugin_opener::open_path(&dir, None::<&str>).map_err(err),
    }
}

#[tauri::command]
async fn review_vscode_available() -> CmdResult<bool> {
    Ok(workspace::vscode().is_some())
}

/// Which language servers tab knows, and whether each is installed here.
#[tauri::command]
async fn lsp_servers() -> CmdResult<Vec<ServerStatus>> {
    tauri::async_runtime::spawn_blocking(lsp::status)
        .await
        .map_err(err)
}

#[tauri::command]
async fn lsp_install(id: String) -> CmdResult<ServerStatus> {
    lsp::install(&id).await.map_err(err)
}

#[derive(Serialize)]
struct LspStarted {
    session: String,
    root_uri: String,
    capabilities: serde_json::Value,
}

/// Starts a language server in the PR's checkout. Notifications arrive as `tab://lsp`.
#[tauri::command]
async fn lsp_start(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    server: String,
    repo: String,
    number: u64,
) -> CmdResult<LspStarted> {
    let dir = state.workspaces.worktree(&repo, number).map_err(err)?;
    if !dir.exists() {
        return Err("This pull request hasn't been checked out on this computer yet.".into());
    }
    let s = state
        .lsp
        .start(&server, &dir, move |e| {
            let _ = app.emit("tab://lsp", &e);
        })
        .await
        .map_err(err)?;
    Ok(LspStarted {
        session: s.id.clone(),
        root_uri: s.root_uri.clone(),
        capabilities: s.capabilities(),
    })
}

#[tauri::command]
async fn lsp_request(
    state: State<'_, AppState>,
    session: String,
    method: String,
    params: serde_json::Value,
) -> CmdResult<serde_json::Value> {
    let s = state
        .lsp
        .get(&session)
        .await
        .ok_or("That language server has stopped.")?;
    s.request(&method, params).await.map_err(err)
}

#[tauri::command]
async fn lsp_notify(
    state: State<'_, AppState>,
    session: String,
    method: String,
    params: serde_json::Value,
) -> CmdResult<()> {
    let s = state
        .lsp
        .get(&session)
        .await
        .ok_or("That language server has stopped.")?;
    s.notify(&method, params).await.map_err(err)
}

#[tauri::command]
async fn lsp_stop(state: State<'_, AppState>, session: String) -> CmdResult<()> {
    state.lsp.stop(&session).await;
    Ok(())
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
    let next = unmask(config, &*state.config.lock().await);
    state.apply(next).await.map(|c| masked(&c)).map_err(err)
}

/// Switches the model chats use. Only the agent hub picks it up, so
/// connectors keep running; the next turn of any chat uses it.
#[tauri::command]
async fn set_agent_model(
    state: State<'_, AppState>,
    model: Option<String>,
) -> CmdResult<TabConfig> {
    let mut current = state.config.lock().await;
    let mut next = current.clone();
    next.agent.model = model
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty());
    next.save(&state.config_path).map_err(err)?;
    *current = next.clone();
    drop(current);
    state.hub.set_config(next.agent.clone()).await;
    let _ = state.events.send(EngineEvent::ConfigChanged);
    Ok(masked(&next))
}

/// Sets the Claude Code permission mode from the chat composer; applies
/// from the next message.
#[tauri::command]
async fn set_agent_mode(state: State<'_, AppState>, mode: String) -> CmdResult<TabConfig> {
    if !["default", "acceptEdits", "plan", "auto"].contains(&mode.as_str()) {
        return Err(format!("unknown permission mode {mode}"));
    }
    let mut current = state.config.lock().await;
    let mut next = current.clone();
    next.agent.permission_mode = mode;
    next.save(&state.config_path).map_err(err)?;
    *current = next.clone();
    drop(current);
    state.hub.set_config(next.agent.clone()).await;
    let _ = state.events.send(EngineEvent::ConfigChanged);
    Ok(masked(&next))
}

/// Saves secrets the person typed into a setup card in the chat, and turns
/// the connection on once everything it needs is set. Only secret fields are
/// accepted here; agents fill in the rest.
#[tauri::command]
async fn connection_set_secrets(
    state: State<'_, AppState>,
    id: String,
    values: serde_json::Map<String, serde_json::Value>,
) -> CmdResult<TabConfig> {
    let descriptor = registry()
        .into_iter()
        .map(|f| f.descriptor())
        .find(|d| d.id == id)
        .ok_or("no such connection")?;
    let mut next = state.config.lock().await.clone();
    let cfg = next.connectors.entry(id.clone()).or_default();
    for (k, v) in values {
        let secret = descriptor
            .settings
            .iter()
            .any(|f| f.key == k && f.kind == SettingKind::Secret);
        if !secret {
            return Err(format!("{k} isn't a secret setting"));
        }
        cfg.settings.insert(k, v);
    }
    cfg.enabled = descriptor.settings.iter().filter(|f| f.required).all(|f| {
        cfg.settings
            .get(&f.key)
            .is_some_and(|v| !v.is_null() && v.as_str().is_none_or(|s| !s.trim().is_empty()))
    });
    state.apply(next).await.map(|c| masked(&c)).map_err(err)
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
            // Search by meaning: follows the setting on the tab connection.
            let (embed, embed_rx) = tokio::sync::watch::channel(tab_core::embed::choice(&config));
            {
                let (store, dir) = (store.clone(), data_dir.clone());
                tauri::async_runtime::spawn(async move {
                    tab_core::embed::spawn_indexer(store, dir, embed_rx);
                });
            }
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
            let core = Arc::new(Core {
                store: store.clone(),
                gateway,
                events: events.clone(),
                engine,
                hub: hub.clone(),
                config: Mutex::new(config.clone()),
                config_path,
                embed,
            });
            let state = AppState {
                core: core.clone(),
                mcp_url: std::sync::Mutex::new(None),
                workspaces: Arc::new(Workspaces::new(data_dir.join("workspaces"))),
                lsp: Arc::new(LspHub::new()),
            };
            tauri::async_runtime::block_on(async {
                state.restart(&config).await;
                // Local MCP server for agents.
                let server = Arc::new(McpServer {
                    store,
                    hub: hub.clone(),
                    engine: Arc::new(LiveEngine(core)),
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
                        // Turns cut off by the last quit carry on now.
                        hub.resume_interrupted();
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
            item_by_url,
            attachment_save,
            image_read,
            start_dictation,
            github_link_summary,
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
            connection_set_secrets,
            set_agent_model,
            set_agent_mode,
            rate_pressure,
            item_context,
            context_add,
            context_update,
            context_remove,
            create_task,
            chat_open,
            chat_send,
            chat_stop,
            projects_list,
            project_save,
            project_delete,
            item_set_project,
            search_index_status,
            agent_activity,
            graph_view,
            graph_node,
            ask_chat_open,
            ask_chat_send,
            project_notes,
            project_note_save,
            project_note_delete,
            review_chat_open,
            review_chat_send,
            approval_resolve,
            detect_agents,
            mcp_info,
            gh_status,
            usage_summary,
            gh_login,
            gh_install,
            github_pull,
            github_file,
            github_review_action,
            review_workspace_prepare,
            review_workspace_file,
            review_workspace_files,
            review_workspace_search,
            review_workspace_open,
            review_vscode_available,
            lsp_servers,
            lsp_install,
            lsp_start,
            lsp_request,
            lsp_notify,
            lsp_stop,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event
                && let Some(state) = app.try_state::<AppState>()
            {
                // Stops agent processes and the OpenCode server tab started.
                state.hub.shutdown();
                let lsp = state.lsp.clone();
                tauri::async_runtime::block_on(lsp.stop_all());
            }
        });
}
