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
use tab_core::gateway::{Call, RateLimitState};
use tab_core::live::SteerCommand;
use tab_core::local_hub::{HubStatusView, LocalHub};
use tab_core::lsp::{self, LspHub, ServerStatus};
use tab_core::mcp::{EngineAccess, McpServer};
use tab_core::model::AgentKind;
use tab_core::model::{
    Approval, ConnectorInfo, ContextEntry, ContextSection, ItemContext, ItemState, NewContext,
    NewTask, Preview, QueueItem, Session, SessionMessage, SettingKind,
};
use tab_core::ratelimit::Pressure;
use tab_core::storage;
use tab_core::store::{AuditEntry, AuditQuery, UsageRow};
use tab_core::workflow::{self, Issue, Run, SavedBy, Workflow};
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
    /// The hub, running inside the app. None if it couldn't open.
    work_hub: Option<Arc<LocalHub>>,
    mcp_url: std::sync::Mutex<Option<String>>,
    workspaces: Arc<Workspaces>,
    lsp: Arc<LspHub>,
    data_dir: PathBuf,
}

impl AppState {
    /// An attachment path as this device has it. Chats synced from another
    /// device, or files swept from the cache, name a path that isn't here;
    /// one in the hub's naming (`.../<hash hex>/<name>`) is fetched from the
    /// hub into this device's attachments folder.
    fn local_attachment(&self, path: &str) -> String {
        let p = std::path::Path::new(path);
        if p.exists() {
            return path.to_string();
        }
        let (Some(hub), Some(file)) = (&self.work_hub, tab_core::live::file_ref(p)) else {
            return path.to_string();
        };
        match hub.file_materialize(&self.hub.attachments_dir(), &file.hash, &file.name) {
            Ok(local) => local.display().to_string(),
            Err(_) => path.to_string(),
        }
    }

    /// Sends the person's message to a session another device runs.
    fn steer_remote(&self, session: &str, text: &str, attachments: &[String]) -> CmdResult<()> {
        let hub = self
            .work_hub
            .as_ref()
            .ok_or("this chat runs on another device, and the hub isn't running here")?;
        let files = tab_core::live::file_refs(attachments);
        if files.len() != attachments.len() {
            return Err("an attachment isn't in the hub, so the other device can't see it".into());
        }
        hub.steer_remote(
            session,
            &SteerCommand::Message {
                text: text.into(),
                files,
            },
        )
        .map_err(err)?;
        Ok(())
    }
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
    async fn pages_changed(&self, connector: &str, pages: &[String]) {
        match self.0.engine.read().await.as_ref() {
            Some(e) => e.pages_changed(connector, pages),
            None => tab_core::connectors::forget_pages(connector, pages),
        }
    }
    async fn save_connector(&self, id: &str, cfg: ConnectorConfig) -> anyhow::Result<()> {
        let mut next = self.0.config.lock().await.clone();
        next.connectors.insert(id.to_string(), cfg);
        self.0.apply(next).await.map(|_| ())
    }
    async fn dry_run_workflow(&self, w: Workflow, item_id: Option<String>) -> anyhow::Result<Run> {
        let guard = self.0.engine.read().await;
        guard
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("engine is not running"))?
            .run_workflow(w, item_id.as_deref(), true)
            .await
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
        let sandbox = tab_core::sandbox::current();
        if sandbox.config() != next.sandbox {
            sandbox.set_config(next.sandbox.clone());
            // OpenCode servers keep the policy they started with.
            self.hub.stop_opencode().await;
        }
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

/// Saves a file the person pasted, dropped or picked in a chat. The body is
/// the raw bytes; the `x-name` header carries the original file name
/// (URI-encoded). Each file gets its own folder so the agent sees its name.
#[tauri::command]
async fn attachment_save(
    state: State<'_, AppState>,
    request: tauri::ipc::Request<'_>,
) -> CmdResult<String> {
    let tauri::ipc::InvokeBody::Raw(bytes) = request.body() else {
        return Err("expected the file's bytes".into());
    };
    if bytes.len() > MAX_ATTACHMENT {
        return Err("that file is over 50 MB".into());
    }
    let name = request
        .headers()
        .get("x-name")
        .and_then(|v| v.to_str().ok())
        .map(percent_decode)
        .unwrap_or_default();
    let name = safe_file_name(&name);
    // Into the hub, so other devices can show it and the agent's machine
    // can fetch it; the attachments folder keeps a copy under its hash.
    if let Some(hub) = &state.work_hub {
        let path = hub
            .attach(&state.hub.attachments_dir(), bytes, &name)
            .map_err(err)?;
        return Ok(path.display().to_string());
    }
    let dir = state
        .hub
        .attachments_dir()
        .join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&dir).map_err(err)?;
    let path = dir.join(name);
    std::fs::write(&path, bytes).map_err(err)?;
    Ok(path.display().to_string())
}

const MAX_IMAGE: usize = 20 * 1024 * 1024;
const MAX_ATTACHMENT: usize = 50 * 1024 * 1024;

fn image_ext(name: &str) -> Option<&'static str> {
    let ext = name.rsplit('.').next()?.to_ascii_lowercase();
    ["png", "jpg", "jpeg", "gif", "webp"]
        .into_iter()
        .find(|e| *e == ext)
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
        match (
            b[i],
            b.get(i + 1).copied().and_then(hex),
            b.get(i + 2).copied().and_then(hex),
        ) {
            (b'%', Some(h), Some(l)) => {
                out.push(h * 16 + l);
                i += 3;
            }
            (c, _, _) => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A file name that is safe on every OS: no folders, no reserved characters.
fn safe_file_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or_default();
    let clean: String = base
        .chars()
        .map(|c| {
            if c.is_control() || "<>:\"|?*".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let clean = clean.trim().trim_matches('.').to_string();
    if clean.is_empty() {
        "file".into()
    } else {
        clean.chars().take(120).collect()
    }
}

/// Opens a file the agent shared or the person attached with its default
/// app. Anything that could run code (scripts, apps, installers) is shown in
/// its folder instead, so a path in a chat can never launch a program.
#[tauri::command]
async fn file_open(path: String, reveal: Option<bool>) -> CmdResult<()> {
    let p = std::path::Path::new(&path);
    if !p.is_absolute() || !p.exists() {
        return Err("that file isn't there any more".into());
    }
    const SAFE: &[&str] = &[
        "png", "jpg", "jpeg", "gif", "webp", "svg", "pdf", "txt", "md", "csv", "tsv", "json",
        "log", "docx", "xlsx", "pptx", "doc", "xls", "ppt", "odt", "ods", "rtf", "zip", "mp4",
        "mov", "webm", "mp3", "wav",
    ];
    let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if reveal.unwrap_or(false) || !p.is_file() || !SAFE.contains(&ext.as_str()) {
        tauri_plugin_opener::reveal_item_in_dir(p).map_err(err)
    } else {
        tauri_plugin_opener::open_path(p, None::<&str>).map_err(err)
    }
}

/// An image file's bytes, for thumbnails and the viewer: attachments, and
/// screenshots an agent mentions by path. Only image files are served.
#[tauri::command]
async fn image_read(state: State<'_, AppState>, path: String) -> CmdResult<tauri::ipc::Response> {
    image_ext(&path).ok_or("not an image")?;
    let path = state.local_attachment(&path);
    let meta = std::fs::metadata(&path).map_err(err)?;
    if !meta.is_file() || meta.len() as usize > MAX_IMAGE {
        return Err("not an image tab can show".into());
    }
    Ok(tauri::ipc::Response::new(
        std::fs::read(&path).map_err(err)?,
    ))
}

/// Extensions the file stage can show inside tab: PDFs, and text it shows as
/// text. Other files open in their default app instead.
const VIEWABLE: &[&str] = &[
    "pdf", "svg", "txt", "md", "markdown", "csv", "tsv", "json", "jsonl", "log", "yaml", "yml",
    "toml", "xml", "html", "htm", "css", "js", "jsx", "ts", "tsx", "mjs", "cjs", "rs", "py", "rb",
    "go", "java", "kt", "swift", "c", "h", "cc", "cpp", "hpp", "cs", "sh", "zsh", "bash", "sql",
    "ini", "env", "conf", "diff", "patch", "lua", "php", "vue", "svelte", "dart", "scala", "ex",
    "exs", "r", "tex",
];
const MAX_VIEWABLE: u64 = 30 * 1024 * 1024;

/// A file's bytes for the file stage, when it is a kind tab can show (see
/// `VIEWABLE`) and not too big. Images go through `image_read`.
#[tauri::command]
async fn file_read(state: State<'_, AppState>, path: String) -> CmdResult<tauri::ipc::Response> {
    let path = state.local_attachment(&path);
    let p = std::path::Path::new(&path);
    let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if !p.is_absolute() || !VIEWABLE.contains(&ext.as_str()) {
        return Err("tab can't show this kind of file".into());
    }
    let meta = std::fs::metadata(p).map_err(|_| "that file isn't there any more".to_string())?;
    if !meta.is_file() || meta.len() > MAX_VIEWABLE {
        return Err("this file is too big to show here".into());
    }
    Ok(tauri::ipc::Response::new(std::fs::read(p).map_err(err)?))
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
    #[cfg(windows)]
    {
        // Windows voice typing is the Win + H shortcut; press it for the person.
        let _ = app;
        press_win_h().map_err(err)
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = app;
        Err("Use your desktop's dictation to talk into the box.".into())
    }
}

/// Synthesizes Win + H so Windows opens its voice typing for the focused box.
#[cfg(windows)]
fn press_win_h() -> Result<(), String> {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput, VK_LWIN,
    };
    const VK_H: u16 = 0x48;
    let key = |vk: u16, up: bool| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: if up { KEYEVENTF_KEYUP } else { 0 },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let inputs = [
        key(VK_LWIN, false),
        key(VK_H, false),
        key(VK_H, true),
        key(VK_LWIN, true),
    ];
    // SAFETY: `inputs` is a valid array of INPUT structs and the size matches.
    let sent = unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            std::mem::size_of::<INPUT>() as i32,
        )
    };
    if sent as usize == inputs.len() {
        Ok(())
    } else {
        Err("Couldn't open Windows voice typing. Press Windows + H instead.".into())
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

/// Tasks that have this link (a PR, say) in their Context.
#[tauri::command]
async fn items_for_link(state: State<'_, AppState>, url: String) -> CmdResult<Vec<QueueItem>> {
    state.store.items_with_context_link(&url).map_err(err)
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
                auto_title: true,
                ..Default::default()
            }).map_err(err))?
        }
    };
    let session = state.hub.session_for_item(&item.id).map_err(err)?;
    // Another device runs this chat: the message goes there through the hub.
    if !state.hub.runs_here(&session) {
        state.steer_remote(&session.id, &text, &attachments)?;
        return Ok(ChatStarted { item, session });
    }
    // A turn in flight takes the message (and its files) as steering;
    // otherwise it starts one.
    if state
        .hub
        .steer_with(&session.id, &text, &attachments)
        .map_err(err)?
    {
        return Ok(ChatStarted { item, session });
    }
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
    if local {
        state.workspaces.mark_used(&target.repo, target.number);
    }
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
    // A turn in flight takes the message as steering; otherwise it starts one.
    if !state
        .hub
        .steer_with(&session.id, &text, &attachments)
        .map_err(err)?
    {
        state
            .hub
            .send_with(&session.id, &text, &attachments)
            .map_err(err)?;
    }
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
    let remote = state
        .store
        .session_get(&session_id)
        .map_err(err)?
        .is_some_and(|s| !state.hub.runs_here(&s));
    match (&state.work_hub, remote) {
        (Some(h), true) => {
            h.steer_remote(&session_id, &SteerCommand::Stop)
                .map_err(err)?;
        }
        _ => state.hub.stop(&session_id),
    }
    Ok(())
}

/// "Send now": stop the tool a turn is waiting on, so the messages the
/// person already sent are read right away.
#[tauri::command]
async fn chat_interrupt(state: State<'_, AppState>, session_id: String) -> CmdResult<()> {
    let remote = state
        .store
        .session_get(&session_id)
        .map_err(err)?
        .is_some_and(|s| !state.hub.runs_here(&s));
    match (&state.work_hub, remote) {
        (Some(h), true) => {
            h.steer_remote(&session_id, &SteerCommand::Interrupt)
                .map_err(err)?;
        }
        _ => {
            state.hub.interrupt(&session_id);
        }
    }
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
    let d = Decision::from_parts(&decision, message.clone(), answers.clone());
    if state.hub.resolve_approval(&id, d) {
        return Ok(true);
    }
    // A prompt from a session another device runs: answer it there.
    let Some(hub) = &state.work_hub else {
        return Ok(false);
    };
    let Some(session) = hub.remote_approval(&id) else {
        return Ok(false);
    };
    hub.steer_remote(
        &session,
        &SteerCommand::Approval {
            id,
            decision,
            message,
            answers,
        },
    )
    .map_err(err)?;
    Ok(true)
}

#[tauri::command]
async fn detect_agents(state: State<'_, AppState>) -> CmdResult<Vec<AgentProbe>> {
    Ok(state.hub.detect().await)
}

/// Every harness tab can drive: names, icons, capabilities, install and models.
#[tauri::command]
fn agent_harnesses() -> Vec<tab_core::agents::catalog::Harness> {
    tab_core::agents::catalog::harnesses().to_vec()
}

/// The models of every installed harness, for the pickers.
#[tauri::command]
async fn agent_models(
    state: State<'_, AppState>,
) -> CmdResult<Vec<tab_core::agents::catalog::ModelOption>> {
    Ok(state.hub.models().await)
}

/// Installs a harness (npm, or its own installer). Output lines arrive as
/// `tab://agent-upgrade` events while it runs.
#[tauri::command]
async fn agent_install(app: tauri::AppHandle, kind: AgentKind) -> CmdResult<()> {
    tab_core::agents::run_install(kind, move |line| {
        let _ = app.emit("tab://agent-upgrade", &line);
    })
    .await
    .map_err(err)
}

/// The configured agent's version, the newest release and how to update it.
#[tauri::command]
async fn agent_health(state: State<'_, AppState>) -> CmdResult<Option<AgentProbe>> {
    Ok(state.hub.health().await)
}

/// Updates the agent CLI the way it was installed. Output lines arrive as
/// `tab://agent-upgrade` events while it runs.
#[tauri::command]
async fn agent_upgrade(app: tauri::AppHandle, state: State<'_, AppState>) -> CmdResult<()> {
    state
        .hub
        .upgrade(move |line| {
            let _ = app.emit("tab://agent-upgrade", &line);
        })
        .await
        .map_err(err)
}

#[derive(Serialize)]
struct McpInfo {
    url: Option<String>,
    token: String,
}

/// Where the hub listens for other devices (the next free port up from here).
const HUB_PORT: u16 = 47_850;

/// For Settings → Hub.
#[tauri::command]
async fn hub_status(state: State<'_, AppState>) -> CmdResult<Option<HubStatusView>> {
    let Some(hub) = state.work_hub.clone() else {
        return Ok(None);
    };
    tauri::async_runtime::spawn_blocking(move || hub.status())
        .await
        .map_err(err)?
        .map(Some)
        .map_err(err)
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
    let exclusive = state.store.exclusive_entities(id).map_err(err)?;
    Ok(serde_json::json!({
        "entity": entity,
        "exclusive": exclusive,
        "connected": connected.iter().map(|(e, n)| serde_json::json!({ "entity": e, "shared": n })).collect::<Vec<_>>(),
        "items": items,
        "notes": notes,
    }))
}

/// Remove nodes from the graph (and keep them out), optionally with the
/// nodes that only exist because of them.
#[tauri::command]
async fn graph_delete(
    state: State<'_, AppState>,
    id: i64,
    with_exclusive: bool,
) -> CmdResult<usize> {
    let mut ids = vec![id];
    if with_exclusive {
        ids.extend(
            state
                .store
                .exclusive_entities(id)
                .map_err(err)?
                .into_iter()
                .map(|e| e.id),
        );
    }
    state.store.entities_delete(&ids, Utc::now()).map_err(err)
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
    // A turn in flight takes the message as steering; otherwise it starts one.
    if !state
        .hub
        .steer_with(&session.id, &text, &attachments)
        .map_err(err)?
    {
        state
            .hub
            .send_with(&session.id, &text, &attachments)
            .map_err(err)?;
    }
    Ok(session)
}

/// Slash commands the agent last reported, for the composer's `/` menu.
#[tauri::command]
fn agent_slash_commands(state: State<'_, AppState>) -> Vec<String> {
    state.hub.slash_commands()
}

/// The state of each item's latest chat (working, needs you, monitoring,
/// failed or idle), for the queue rows and the nav. Items with no chat are
/// left out.
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
        // A chat marked done no longer asks for the person: keep it off the
        // Work badge and the list's agent dots.
        if matches!(state.store.get(&item), Ok(Some(i)) if i.state == ItemState::Done) {
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

// --- Storage -------------------------------------------------------------

/// A closed or merged PR's checkout stays this long after it was last opened,
/// so one being looked at right as it merges isn't pulled out from under you.
const CHECKOUT_GRACE: chrono::Duration = chrono::Duration::days(1);
/// How often the cleanup sweep runs.
const SWEEP_EVERY: std::time::Duration = std::time::Duration::from_secs(6 * 3600);

#[derive(Serialize)]
struct CheckoutUsage {
    repo: String,
    number: u64,
    last_used: chrono::DateTime<Utc>,
    bytes: u64,
}

#[derive(Serialize)]
struct StorageUsage {
    attachments: storage::Size,
    sessions: storage::Size,
    workspaces: storage::Size,
    checkouts: Vec<CheckoutUsage>,
}

/// Whether a PR has closed or merged. Errors (no GitHub, offline) say no.
async fn pr_closed(gateway: &Gateway, repo: &str, number: u64) -> bool {
    #[derive(serde::Deserialize)]
    struct Pr {
        state: String,
    }
    let url = format!("https://api.github.com/repos/{repo}/pulls/{number}");
    let call = Call::get("github", "github", "review checkout cleanup", url)
        .cache(std::time::Duration::from_secs(3600));
    let res = tab_core::ratelimit::background(gateway.send(call)).await;
    match res
        .map_err(anyhow::Error::from)
        .and_then(|r| r.json::<Pr>())
    {
        Ok(pr) => pr.state != "open",
        Err(e) => {
            tracing::debug!(%repo, number, error = %e, "couldn't read PR state");
            false
        }
    }
}

/// Removes review checkouts whose PR has closed, or that nobody has opened
/// for `keep`.
async fn sweep_checkouts(
    ws: &Workspaces,
    gateway: &Gateway,
    keep: std::time::Duration,
) -> storage::Swept {
    let now = Utc::now();
    let keep = chrono::Duration::from_std(keep).unwrap_or(chrono::Duration::days(7));
    let mut swept = storage::Swept::default();
    for c in ws.checkouts() {
        let idle = now - c.last_used;
        let gone =
            idle > keep || (idle > CHECKOUT_GRACE && pr_closed(gateway, &c.repo, c.number).await);
        if gone {
            swept.add(remove_checkout(ws, &c.repo, c.number).await);
        }
    }
    swept
}

/// Something the size limit can remove.
#[derive(Debug, Clone, PartialEq)]
enum Evictable {
    Attachment(PathBuf),
    Checkout(String, u64),
}

/// Total size of everything the storage settings cover.
fn storage_total(hub: &AgentHub, data_dir: &std::path::Path) -> u64 {
    [
        hub.attachments_dir(),
        hub.sessions_dir(),
        data_dir.join("workspaces"),
    ]
    .iter()
    .map(|d| storage::dir_size(d).bytes)
    .sum()
}

/// Past the size limit, removes the least recently used attachments and
/// review checkouts until it fits.
async fn enforce_limit(
    hub: Arc<AgentHub>,
    ws: Arc<Workspaces>,
    data_dir: PathBuf,
    limit: u64,
) -> storage::Swept {
    let mut swept = storage::Swept::default();
    // Removing a repository's last checkout also frees its clone, which the
    // plan can't see, so re-measure and go again if still over.
    for _ in 0..3 {
        let (h, w, d) = (hub.clone(), ws.clone(), data_dir.clone());
        let plan = tauri::async_runtime::spawn_blocking(move || {
            let now = Utc::now();
            let total = storage_total(&h, &d);
            if total <= limit {
                return Vec::new();
            }
            let mut candidates: Vec<storage::Candidate<Evictable>> = h
                .attachments(now)
                .unwrap_or_default()
                .into_iter()
                .map(|c| storage::Candidate {
                    key: Evictable::Attachment(c.key),
                    last_used: c.last_used,
                    bytes: c.bytes,
                })
                .collect();
            candidates.extend(w.checkouts().into_iter().map(|c| {
                storage::Candidate {
                    bytes: w
                        .worktree(&c.repo, c.number)
                        .map(|p| storage::dir_size(&p).bytes)
                        .unwrap_or(0),
                    key: Evictable::Checkout(c.repo, c.number),
                    last_used: c.last_used,
                }
            }));
            let grace = chrono::Duration::from_std(storage::EVICT_GRACE).unwrap_or_default();
            storage::evict(candidates, total, limit, now - grace)
        })
        .await
        .unwrap_or_default();
        if plan.is_empty() {
            break;
        }
        for c in plan {
            swept.add(match c.key {
                Evictable::Attachment(p) => storage::remove_path(&p),
                Evictable::Checkout(repo, n) => remove_checkout(&ws, &repo, n).await,
            });
        }
    }
    if swept.removed > 0 {
        tracing::info!(
            removed = swept.removed,
            bytes = swept.bytes,
            "evicted to fit the storage limit"
        );
    }
    swept
}

async fn remove_checkout(ws: &Workspaces, repo: &str, number: u64) -> storage::Swept {
    let Ok(dir) = ws.worktree(repo, number) else {
        return storage::Swept::default();
    };
    let bytes = storage::dir_size(&dir).bytes;
    match ws.remove(repo, number).await {
        Ok(()) => {
            tracing::info!(%repo, number, bytes, "removed review checkout");
            storage::Swept { removed: 1, bytes }
        }
        Err(e) => {
            tracing::warn!(%repo, number, error = %e, "couldn't remove review checkout");
            storage::Swept::default()
        }
    }
}

/// Clears out attachments, chat folders and review checkouts by the
/// Settings → Storage rules: unused for `keep_days`, then past `max_gb`.
async fn sweep_storage(core: &Core, ws: Arc<Workspaces>, data_dir: PathBuf) -> storage::Swept {
    let cfg = core.config.lock().await.storage.clone();
    let hub = core.hub.clone();
    let keep = cfg.keep();
    let h = hub.clone();
    let mut swept = tauri::async_runtime::spawn_blocking(move || h.sweep_files(Utc::now(), keep))
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r)
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "sweeping chat files failed");
            storage::Swept::default()
        });
    swept.add(sweep_checkouts(&ws, &core.gateway, keep).await);
    // Shared dependency installs no checkout links to any more.
    let live: Vec<PathBuf> = ws
        .checkouts()
        .iter()
        .filter_map(|c| ws.worktree(&c.repo, c.number).ok())
        .collect();
    swept.add(ws.deps().sweep(&live, keep).await);
    if let Some(limit) = cfg.max_bytes() {
        swept.add(enforce_limit(hub, ws, data_dir, limit).await);
    }
    swept
}

/// How much space tab's files take, for Settings → Storage.
#[tauri::command]
async fn storage_usage(state: State<'_, AppState>) -> CmdResult<StorageUsage> {
    let hub = state.hub.clone();
    let ws = state.workspaces.clone();
    let data = state.data_dir.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let checkouts = ws
            .checkouts()
            .into_iter()
            .map(|c| CheckoutUsage {
                bytes: ws
                    .worktree(&c.repo, c.number)
                    .map(|d| storage::dir_size(&d).bytes)
                    .unwrap_or(0),
                repo: c.repo,
                number: c.number,
                last_used: c.last_used,
            })
            .collect();
        StorageUsage {
            attachments: storage::dir_size(&hub.attachments_dir()),
            sessions: storage::dir_size(&hub.sessions_dir()),
            workspaces: storage::dir_size(&data.join("workspaces")),
            checkouts,
        }
    })
    .await
    .map_err(err)
}

/// Runs the cleanup sweep now.
#[tauri::command]
async fn storage_clean(state: State<'_, AppState>) -> CmdResult<storage::Swept> {
    Ok(sweep_storage(
        &state.core,
        state.workspaces.clone(),
        state.data_dir.clone(),
    )
    .await)
}

/// Removes one review checkout, or every one when `repo` is empty. They come
/// back the next time their PR is opened.
#[tauri::command]
async fn storage_remove_checkouts(
    state: State<'_, AppState>,
    repo: Option<String>,
    number: Option<u64>,
) -> CmdResult<storage::Swept> {
    let mut swept = storage::Swept::default();
    for c in state.workspaces.checkouts() {
        let pick = match (&repo, number) {
            (Some(r), Some(n)) => *r == c.repo && n == c.number,
            _ => true,
        };
        if pick {
            swept.add(remove_checkout(&state.workspaces, &c.repo, c.number).await);
        }
    }
    Ok(swept)
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

/// Your open pull requests, from GitHub's search.
#[tauri::command]
async fn github_my_pulls(state: State<'_, AppState>) -> CmdResult<Vec<github_review::MyPull>> {
    github_review::my_pulls(&state.gateway).await.map_err(err)
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
    state.workspaces.mark_used(&repo, number);
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

/// Installs (or reuses) the checkout's dependencies, shared between
/// checkouts by lockfile, and links them in. See `tab_core::deps`.
#[tauri::command]
async fn deps_prepare(
    state: State<'_, AppState>,
    repo: String,
    number: u64,
    ecosystem: String,
) -> CmdResult<tab_core::deps::DepsStatus> {
    let dir = state.workspaces.worktree(&repo, number).map_err(err)?;
    if !dir.exists() {
        return Err("This pull request hasn't been checked out on this computer yet.".into());
    }
    match ecosystem.as_str() {
        "js" => state.workspaces.deps().prepare_js(&dir).await.map_err(err),
        other => Err(format!("No dependency support for {other}")),
    }
}

/// Type declarations from the checkout's node_modules, for the editor's
/// built-in TypeScript service: `[path, text]` with paths under `node_modules/`.
#[tauri::command]
async fn deps_types(
    state: State<'_, AppState>,
    repo: String,
    number: u64,
) -> CmdResult<Vec<(String, String)>> {
    let dir = state.workspaces.worktree(&repo, number).map_err(err)?;
    let ws = state.workspaces.clone();
    tauri::async_runtime::spawn_blocking(move || ws.deps().types(&dir, 24 * 1024 * 1024))
        .await
        .map_err(err)
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

// --- Workflows -------------------------------------------------------------

#[tauri::command]
async fn workflows_list(state: State<'_, AppState>) -> CmdResult<Vec<Workflow>> {
    state.store.workflows().map_err(err)
}

#[tauri::command]
async fn workflow_templates() -> CmdResult<Vec<Workflow>> {
    Ok(workflow::templates())
}

#[tauri::command]
async fn workflow_validate(
    state: State<'_, AppState>,
    workflow: Workflow,
) -> CmdResult<Vec<Issue>> {
    let catalog = with_engine!(state, e => e.connectors());
    let saved = state.store.workflows().map_err(err)?;
    Ok(workflow::validate(&workflow, &catalog, &saved))
}

#[derive(Serialize)]
struct SavedWorkflow {
    workflow: Workflow,
    issues: Vec<Issue>,
}

/// Saves the person's edits. A workflow with errors is saved turned off.
#[tauri::command]
async fn workflow_save(state: State<'_, AppState>, workflow: Workflow) -> CmdResult<SavedWorkflow> {
    let catalog = with_engine!(state, e => e.connectors());
    let saved = state.store.workflows().map_err(err)?;
    let (w, issues) = workflow::prepare_save(workflow, SavedBy::Person, &catalog, &saved);
    let w = state
        .store
        .workflow_save(&w, "user", Utc::now())
        .map_err(err)?;
    let _ = state.events.send(EngineEvent::WorkflowsChanged {
        id: w.id.clone(),
        by: "user".into(),
    });
    Ok(SavedWorkflow {
        workflow: w,
        issues,
    })
}

/// Turns a workflow on or off, or allows its destructive steps. Turning on
/// refuses a workflow that has errors.
#[tauri::command]
async fn workflow_set_enabled(
    state: State<'_, AppState>,
    id: String,
    enabled: bool,
    allow_destructive: Option<bool>,
    allow_commands: Option<bool>,
) -> CmdResult<Workflow> {
    let mut w = state
        .store
        .workflow_get(&id)
        .map_err(err)?
        .ok_or("no such workflow")?;
    if enabled {
        let catalog = with_engine!(state, e => e.connectors());
        let saved = state.store.workflows().map_err(err)?;
        let mut check = w.clone();
        check.enabled = true;
        if let Some(i) = workflow::validate(&check, &catalog, &saved)
            .into_iter()
            .find(|i| i.error)
        {
            return Err(i.message);
        }
    }
    let changed_destructive = allow_destructive.is_some_and(|a| a != w.allow_destructive)
        || allow_commands.is_some_and(|a| a != w.allow_commands);
    w.enabled = enabled;
    if let Some(a) = allow_destructive {
        w.allow_destructive = a;
    }
    if let Some(a) = allow_commands {
        w.allow_commands = a;
    }
    let w = if changed_destructive {
        state
            .store
            .workflow_save(&w, "user", Utc::now())
            .map_err(err)?
    } else {
        state
            .store
            .workflow_set_enabled(&id, enabled)
            .map_err(err)?
            .ok_or("no such workflow")?
    };
    let _ = state.events.send(EngineEvent::WorkflowsChanged {
        id: w.id.clone(),
        by: "user".into(),
    });
    Ok(w)
}

#[tauri::command]
async fn workflow_delete(state: State<'_, AppState>, id: String) -> CmdResult<()> {
    state.store.workflow_delete(&id).map_err(err)?;
    let _ = state.events.send(EngineEvent::WorkflowsChanged {
        id,
        by: "user".into(),
    });
    Ok(())
}

#[tauri::command]
async fn workflow_runs(
    state: State<'_, AppState>,
    workflow_id: Option<String>,
    limit: Option<usize>,
) -> CmdResult<Vec<Run>> {
    state
        .store
        .workflow_runs(workflow_id.as_deref(), limit.unwrap_or(50).min(500))
        .map_err(err)
}

#[tauri::command]
async fn workflow_versions(
    state: State<'_, AppState>,
    id: String,
) -> CmdResult<Vec<serde_json::Value>> {
    Ok(state
        .store
        .workflow_versions(&id)
        .map_err(err)?
        .into_iter()
        .map(|(version, by, at, w)| serde_json::json!({ "version": version, "saved_by": by, "saved_at": at, "workflow": w }))
        .collect())
}

/// Runs a workflow now: dry (nothing changes) or for real.
#[tauri::command]
async fn workflow_run(
    state: State<'_, AppState>,
    workflow: Workflow,
    item_id: Option<String>,
    dry_run: bool,
) -> CmdResult<Run> {
    let guard = state.engine.read().await;
    let e = guard.as_ref().ok_or("engine is not running")?;
    e.run_workflow(workflow, item_id.as_deref(), dry_run)
        .await
        .map_err(err)
}

#[tauri::command]
async fn item_activity(
    state: State<'_, AppState>,
    id: String,
) -> CmdResult<Vec<tab_core::model::Activity>> {
    state.store.activity_for(&id, 200).map_err(err)
}

/// The builder assistant's chat for one workflow (`None` or "" for a new draft).
#[tauri::command]
async fn workflow_chat_open(
    state: State<'_, AppState>,
    workflow_id: Option<String>,
) -> CmdResult<Chat> {
    let key = tab_core::workflow::chat_key(workflow_id.as_deref().unwrap_or(""));
    let session = state.store.session_for_key(&key).map_err(err)?;
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

/// A message to the builder's assistant, with the workflow that's open.
#[tauri::command]
async fn workflow_chat_send(
    state: State<'_, AppState>,
    text: String,
    open: Option<Workflow>,
) -> CmdResult<Session> {
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err("message is empty".into());
    }
    if state.hub.config().kind.is_none() {
        return Err("Set up an agent first (Settings → Agent).".into());
    }
    // The open workflow goes in the brief (refreshed every turn), not in the
    // person's message, so the transcript shows only what they typed.
    let open_now = match &open {
        Some(w) => format!(
            "\n\nOpen in the builder now ({}):\n```json\n{}\n```",
            if w.id.is_empty() {
                "a new draft, not saved yet; leave id out when you save it"
            } else {
                "saved; pass its id to change it"
            },
            serde_json::to_string_pretty(w).map_err(err)?
        ),
        None => "\n\nNothing is open yet: this is a new workflow.".into(),
    };
    let brief = "This chat is the assistant in tab's workflow builder. The person describes what they want \
        automated and you build it as a tab workflow: a trigger and ordered steps that call connection actions. \
        Start with tab_list_actions to see what each connection can start on and do; never invent actions or \
        inputs. Save with tab_save_workflow (pass the open workflow's id to change it, leave id out for a new one); \
        the builder shows what you save right away. Then dry-run it with tab_test_workflow on a real item from \
        tab_search when one fits, and say in a sentence or two what it does and what the dry run showed. \
        Prefer deterministic steps: filters and conditions on fields over anything vague. Keep triggers narrow \
        (a connection and kinds) so workflows can't feed each other. You can't turn workflows on or allow \
        destructive steps (merge, close, move mail); the person does that in the builder, so tell them when it's needed. \
        This chat is about the one workflow open below; work on others only if the person asks.";
    let brief = format!("{brief}{open_now}");
    let title = match open
        .as_ref()
        .map(|w| w.name.trim())
        .filter(|n| !n.is_empty())
    {
        Some(n) => format!("Workflow builder: {n}"),
        None => "Workflow builder".into(),
    };
    let cwd = state.hub.workdir();
    let session = state
        .hub
        .session_for_key(
            &tab_core::workflow::chat_key(open.as_ref().map_or("", |w| w.id.as_str())),
            &title,
            &cwd,
            &brief,
        )
        .map_err(err)?;
    state.hub.send_with(&session.id, &text, &[]).map_err(err)?;
    Ok(session)
}

#[tauri::command]
async fn mark_done(state: State<'_, AppState>, id: String) -> CmdResult<Option<QueueItem>> {
    with_engine!(state, e => e.mark_done(&id).await.map_err(err))
}

/// Why an item shouldn't be marked done yet, from tab and every connector.
/// Adds the one thing only the app knows: an approval still waiting in chat.
#[tauri::command]
async fn done_guards(
    state: State<'_, AppState>,
    id: String,
) -> CmdResult<Vec<tab_core::model::DoneGuard>> {
    let mut guards = with_engine!(state, e => e.done_guards(&id).await.map_err(err))?;
    let waiting = state
        .store
        .session_for_item(&id)
        .map_err(err)?
        .is_some_and(|s| !state.hub.pending_approvals(&s.id).is_empty());
    if waiting && !guards.iter().any(|g| g.source == "agent") {
        guards.insert(
            0,
            tab_core::model::DoneGuard {
                source: "agent".into(),
                level: tab_core::model::GuardLevel::Confirm,
                title: "The agent is waiting on you".into(),
                detail: None,
                url: None,
            },
        );
    }
    Ok(guards)
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
async fn rename_item(
    state: State<'_, AppState>,
    id: String,
    title: String,
) -> CmdResult<Option<QueueItem>> {
    with_engine!(state, e => e.rename(&id, &title).map_err(err))
}

/// The titler's built-in rules, shown under the person's own in Settings.
#[tauri::command]
fn titler_default_rules() -> &'static str {
    tab_core::agents::TITLER_DEFAULT_RULES
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

/// How the sandbox works on this computer, for Settings.
#[tauri::command]
async fn sandbox_status() -> CmdResult<tab_core::sandbox::Status> {
    Ok(tab_core::sandbox::current().status().await)
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
    agent: Option<AgentKind>,
    for_new_chats: Option<bool>,
) -> CmdResult<TabConfig> {
    let mut current = state.config.lock().await;
    let mut next = current.clone();
    // A model on another harness, picked before a chat starts, makes that
    // harness the one new chats use.
    if for_new_chats == Some(true)
        && let Some(kind) = agent
    {
        next.agent.kind = Some(kind);
        next.agent.model = next.agent.models.get(kind.as_str()).cloned();
    }
    let Some(kind) = agent.or(next.agent.kind) else {
        return Err(err(anyhow::anyhow!("no agent is set up")));
    };
    next.agent.set_model(kind, model);
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
/// The in-app browser: one native child webview per browser tab, laid over the
/// page's placeholder. A real webview (not an iframe) so sites that refuse
/// framing, GitHub among them, still load.
#[derive(Deserialize, Clone, Copy)]
struct Bounds {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

#[derive(Serialize, Clone)]
struct BrowserEvent {
    id: String,
    kind: &'static str,
    value: String,
}

fn browser_label(id: &str) -> String {
    format!("browser-{id}")
}

fn browser_view(app: &tauri::AppHandle, id: &str) -> Result<tauri::Webview, String> {
    app.get_webview(&browser_label(id))
        .ok_or_else(|| "browser tab is closed".to_string())
}

#[tauri::command]
fn browser_open(
    app: tauri::AppHandle,
    id: String,
    url: String,
    bounds: Bounds,
) -> Result<(), String> {
    use tauri::webview::{PageLoadEvent, WebviewBuilder};
    use tauri::{LogicalPosition, LogicalSize, WebviewUrl};
    let parsed: tauri::Url = url.parse().map_err(|e| format!("not a web address: {e}"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("only web addresses open in the browser".into());
    }
    let window = app.get_window("main").ok_or("no main window")?;
    let (a, b) = (app.clone(), app.clone());
    let (tab_a, tab_b) = (id.clone(), id.clone());
    let builder = WebviewBuilder::new(browser_label(&id), WebviewUrl::External(parsed))
        .on_page_load(move |wv, payload| {
            let kind = if matches!(payload.event(), PageLoadEvent::Started) {
                "loading"
            } else {
                "loaded"
            };
            let _ = a.emit(
                "tab://browser",
                BrowserEvent {
                    id: tab_a.clone(),
                    kind: "url",
                    value: wv.url().map(|u| u.to_string()).unwrap_or_default(),
                },
            );
            let _ = a.emit(
                "tab://browser",
                BrowserEvent {
                    id: tab_a.clone(),
                    kind,
                    value: String::new(),
                },
            );
        })
        .on_document_title_changed(move |_, title| {
            let _ = b.emit(
                "tab://browser",
                BrowserEvent {
                    id: tab_b.clone(),
                    kind: "title",
                    value: title,
                },
            );
        });
    window
        .add_child(
            builder,
            LogicalPosition::new(bounds.x, bounds.y),
            LogicalSize::new(bounds.w.max(1.0), bounds.h.max(1.0)),
        )
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn browser_bounds(app: tauri::AppHandle, id: String, bounds: Bounds) -> Result<(), String> {
    use tauri::{LogicalPosition, LogicalSize};
    let wv = browser_view(&app, &id)?;
    wv.set_position(LogicalPosition::new(bounds.x, bounds.y))
        .map_err(|e| e.to_string())?;
    wv.set_size(LogicalSize::new(bounds.w.max(1.0), bounds.h.max(1.0)))
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn browser_visible(app: tauri::AppHandle, id: String, visible: bool) -> Result<(), String> {
    let wv = browser_view(&app, &id)?;
    if visible { wv.show() } else { wv.hide() }.map_err(|e| e.to_string())
}

/// `action`: back, forward, reload, or `go` with a url.
#[tauri::command]
fn browser_nav(
    app: tauri::AppHandle,
    id: String,
    action: String,
    url: Option<String>,
) -> Result<(), String> {
    let wv = browser_view(&app, &id)?;
    match action.as_str() {
        "back" => wv.eval("history.back()"),
        "forward" => wv.eval("history.forward()"),
        "reload" => wv.eval("location.reload()"),
        "go" => {
            let u: tauri::Url = url
                .ok_or("no address")?
                .parse()
                .map_err(|e| format!("not a web address: {e}"))?;
            if !matches!(u.scheme(), "http" | "https") {
                return Err("only web addresses open in the browser".into());
            }
            wv.navigate(u)
        }
        _ => return Err("unknown action".into()),
    }
    .map_err(|e| e.to_string())
}

#[tauri::command]
fn browser_close(app: tauri::AppHandle, id: String) -> Result<(), String> {
    match app.get_webview(&browser_label(&id)) {
        Some(wv) => wv.close().map_err(|e| e.to_string()),
        None => Ok(()),
    }
}

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
            lsp::set_tools_dir(data_dir.join("tools"));
            let config_path = app.path().app_config_dir()?.join("config.json");
            let config = TabConfig::load(&config_path)?;
            // Before anything starts a process: agents, language servers
            // and workflow commands all go through it.
            tab_core::sandbox::install(Arc::new(tab_core::sandbox::Sandbox::new(
                config.sandbox.clone(),
                Some(data_dir.clone()),
                config_path.parent().map(Into::into),
            )));
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
            // The hub: this device's copy (tab.db) syncs through its change
            // log, and other devices connect to it on localhost.
            let work_hub = match LocalHub::open(&data_dir.join("hub.db")) {
                Ok(h) => {
                    // Other devices steer the sessions this one runs.
                    h.attach_agents(hub.clone());
                    Some(h)
                }
                Err(e) => {
                    tracing::error!(error = %e, "hub failed to open; running without it");
                    None
                }
            };
            let state = AppState {
                core: core.clone(),
                work_hub: work_hub.clone(),
                mcp_url: std::sync::Mutex::new(None),
                workspaces: Arc::new(Workspaces::new(data_dir.join("workspaces"))),
                lsp: Arc::new(LspHub::new()),
                data_dir: data_dir.clone(),
            };
            // Clean out old attachments, chat folders and review checkouts:
            // shortly after launch, then every few hours.
            {
                let (core, ws, data) = (
                    state.core.clone(),
                    state.workspaces.clone(),
                    state.data_dir.clone(),
                );
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_secs(120)).await;
                    loop {
                        let s = sweep_storage(&core, ws.clone(), data.clone()).await;
                        if s.removed > 0 {
                            tracing::info!(removed = s.removed, bytes = s.bytes, "storage sweep");
                        }
                        tokio::time::sleep(SWEEP_EVERY).await;
                    }
                });
            }
            tauri::async_runtime::block_on(async {
                if let Some(h) = &work_hub {
                    h.follow(store.clone(), events.clone());
                    match h.serve(HUB_PORT, config.mcp.token.clone()).await {
                        Ok(url) => tracing::info!(%url, "hub listening"),
                        Err(e) => tracing::error!(error = %e, "hub listener failed to start"),
                    }
                }
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
            sandbox_status,
            rename_item,
            titler_default_rules,
            browser_open,
            browser_bounds,
            browser_visible,
            browser_nav,
            browser_close,
            list_items,
            search_items,
            item_detail,
            item_by_url,
            attachment_save,
            image_read,
            file_read,
            file_open,
            start_dictation,
            github_link_summary,
            github_my_pulls,
            item_preview,
            mark_done,
            done_guards,
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
            chat_interrupt,
            projects_list,
            project_save,
            project_delete,
            item_set_project,
            search_index_status,
            agent_activity,
            items_for_link,
            agent_slash_commands,
            agent_health,
            agent_upgrade,
            graph_view,
            graph_node,
            graph_delete,
            ask_chat_open,
            ask_chat_send,
            project_notes,
            project_note_save,
            project_note_delete,
            review_chat_open,
            review_chat_send,
            approval_resolve,
            detect_agents,
            agent_harnesses,
            agent_models,
            agent_install,
            mcp_info,
            hub_status,
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
            storage_usage,
            storage_clean,
            storage_remove_checkouts,
            review_vscode_available,
            lsp_servers,
            lsp_install,
            lsp_start,
            deps_prepare,
            deps_types,
            lsp_request,
            lsp_notify,
            lsp_stop,
            workflows_list,
            workflow_templates,
            workflow_validate,
            workflow_save,
            workflow_set_enabled,
            workflow_delete,
            workflow_runs,
            workflow_versions,
            workflow_run,
            item_activity,
            workflow_chat_open,
            workflow_chat_send,
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
