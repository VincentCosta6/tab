//! tab's desktop backend, without a UI.
//!
//! [`Backend::boot`] opens the store and the hub, starts the engine, the
//! agent hub, the project indexer and the local MCP server, the way the
//! Tauri shell's setup did. A UI holds the [`Backend`], calls tab-core
//! through it, and subscribes to [`Backend::events`]. Nothing here knows
//! about windows.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use tab_core::agents::{AgentHub, McpEndpoint, coordinator};
use tab_core::config::ConnectorConfig;
use tab_core::local_hub::LocalHub;
use tab_core::lsp::{self, LspHub};
use tab_core::mcp::{EngineAccess, McpServer};
use tab_core::model::{ConnectorInfo, NewTask, Preview, QueueItem};
use tab_core::workflow::{self, Run, Workflow};
use tab_core::workspace::Workspaces;
use tab_core::{Engine, EngineEvent, EngineHandle, Gateway, Store, TabConfig};
use tokio::sync::{Mutex, RwLock, broadcast};

pub mod chat;
pub mod logging;
pub mod paths;
pub mod projects;
pub mod settings;
pub mod storage;
mod vault;
pub mod workflows;

pub use paths::Paths;

pub type SharedEngine = Arc<RwLock<Option<EngineHandle>>>;

/// Everything the UI reaches: the store, the running engine, agents,
/// settings and the hub.
pub struct Backend {
    pub store: Arc<Store>,
    pub gateway: Arc<Gateway>,
    pub events: broadcast::Sender<EngineEvent>,
    pub engine: SharedEngine,
    pub agents: Arc<AgentHub>,
    pub config: Mutex<TabConfig>,
    pub config_path: PathBuf,
    /// Which embedding model search by meaning wants (None when it's off).
    embed: tokio::sync::watch::Sender<Option<&'static str>>,
    /// The hub's leases, once the hub is open, so the engine shares
    /// workflow triggers with the person's other runners.
    leases: std::sync::Mutex<Option<Arc<dyn tab_core::leases::Leases>>>,
    /// The hub, running inside the app. None if it couldn't open.
    pub work_hub: Option<Arc<LocalHub>>,
    pub mcp_url: std::sync::Mutex<Option<String>>,
    pub workspaces: Arc<Workspaces>,
    pub lsp: Arc<LspHub>,
    pub paths: Paths,
}

/// An error for the UI to show.
pub fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

impl Backend {
    /// Starts everything. Call from inside a multi-threaded tokio runtime.
    pub async fn boot(paths: Paths) -> anyhow::Result<Arc<Backend>> {
        logging::open_file(&paths.log_dir);
        // Opened from Finder, tab gets a soft limit of 256 open files, and
        // every agent it starts inherits it.
        tab_core::limits::raise_open_file_limit();
        let data_dir = paths.data_dir.clone();
        std::fs::create_dir_all(&data_dir)?;
        std::fs::create_dir_all(&paths.config_dir)?;
        lsp::set_tools_dir(data_dir.join("tools"));
        let config_path = paths.config_dir.join("config.json");
        tab_core::secrets::install(&config_path);
        let mut config = TabConfig::load(&config_path)?;
        config.mcp.port = paths.ports.mcp_for(config.mcp.port);
        // Before anything starts a process: agents, language servers and
        // workflow commands all go through it.
        tab_core::sandbox::install(Arc::new(
            tab_core::sandbox::Sandbox::new(
                config.sandbox.clone(),
                Some(data_dir.clone()),
                Some(paths.config_dir.clone()),
            )
            .guarding(paths.other_app_dirs()),
        ));
        let store = Arc::new(Store::open(&data_dir.join("tab.db"))?);
        let gateway = Arc::new(Gateway::new(store.clone()));
        // Search by meaning: follows the setting on the tab connection.
        let (embed, embed_rx) = tokio::sync::watch::channel(tab_core::embed::choice(&config));
        tab_core::embed::spawn_indexer(store.clone(), data_dir.clone(), embed_rx);
        let (events, _) = broadcast::channel(1024);

        let agents = AgentHub::new(
            store.clone(),
            events.clone(),
            config.agent.clone(),
            data_dir.clone(),
        );
        agents.remote_mcp().set_servers(config.mcp_servers.clone());

        // The hub: this device's copy (tab.db) syncs through its change
        // log, and other devices connect to it on localhost.
        let hub_path = data_dir.join("hub.db");
        let work_hub = match tokio::task::spawn_blocking(move || open_hub(&hub_path)).await? {
            Ok(h) => Some(h),
            Err(e) => {
                tracing::error!(error = %e, "hub failed to open; running without it");
                None
            }
        };

        let backend = Arc::new(Backend {
            store: store.clone(),
            gateway: gateway.clone(),
            events: events.clone(),
            engine: Arc::new(RwLock::new(None)),
            agents: agents.clone(),
            config: Mutex::new(config.clone()),
            config_path,
            embed,
            leases: Default::default(),
            work_hub: work_hub.clone(),
            mcp_url: std::sync::Mutex::new(None),
            workspaces: Arc::new(Workspaces::new(data_dir.join("workspaces"))),
            lsp: Arc::new(LspHub::new()),
            paths: paths.clone(),
        });

        if let Some(h) = &work_hub {
            // Other devices steer the sessions this one runs.
            h.attach_agents(agents.clone());
            *backend.leases.lock().unwrap() = Some(h.clone());
            // Agent jobs from workflows: this computer takes the ones it
            // can, on the hub's leases.
            let name = tab_core::jobs::computer_name();
            let runner = tab_core::jobs::JobRunner::new(
                store.clone(),
                Arc::new(agents.clone()),
                h.clone(),
                tab_core::runner::RunnerKind::Computer,
                &name,
            );
            runner.start(tab_core::jobs::CancellationToken::new());
        }

        // The end-to-end encrypted vault: its key never leaves this
        // device, and its sealed files go into the hub.
        {
            let v = tab_core::e2e::E2e::new(
                store.clone(),
                &data_dir,
                Arc::new(tab_core::e2e::presence::System),
            );
            if let Some(h) = &work_hub {
                v.set_files(Arc::new(vault::HubFiles(h.clone())));
            }
            agents.set_vault(v);
        }

        // What projects know: indexed here, from the hub's files, the email
        // connection and the web.
        tab_core::knowledge::spawn(
            store.clone(),
            gateway.clone(),
            Arc::new(KnowledgeSources {
                hub: work_hub.clone(),
                backend: Arc::downgrade(&backend),
            }),
        );

        tracing::info!(
            mode = ?config.hub.mode,
            cloud_hub = ?config.hub.cloud_hub,
            hub_open = work_hub.is_some(),
            "hub: where work is kept"
        );
        // TODO(gpui): syncing with tab cloud needs the account crate's link,
        // which still lives in src-tauri (account.rs). Until it moves here,
        // a computer that moved its work to tab cloud keeps using hub.db.

        if let Some(h) = &work_hub {
            h.follow(store.clone(), events.clone());
            match h.serve(paths.ports.hub, config.mcp.token.clone()).await {
                Ok(url) => tracing::info!(%url, "hub listening"),
                Err(e) => tracing::error!(error = %e, "hub listener failed to start"),
            }
        }
        backend.restart(&config).await;

        // Local MCP server for agents.
        let server = Arc::new(McpServer {
            store,
            hub: agents.clone(),
            engine: Arc::new(LiveEngine(Arc::downgrade(&backend))),
            events,
            token: config.mcp.token.clone(),
        });
        match tab_core::mcp::serve(server, config.mcp.port).await {
            Ok(url) => {
                tracing::info!(%url, "MCP server listening");
                agents.set_mcp(McpEndpoint {
                    url: url.clone(),
                    token: config.mcp.token.clone(),
                });
                *backend.mcp_url.lock().unwrap() = Some(url);
                // Turns cut off by the last quit carry on now.
                agents.resume_interrupted();
                coordinator::spawn_monitor(agents.clone());
            }
            Err(e) => tracing::error!(error = %e, "MCP server failed to start"),
        }
        storage::spawn_sweeps(Arc::downgrade(&backend));
        Ok(backend)
    }

    /// Starts the engine again with these settings.
    pub async fn restart(&self, cfg: &TabConfig) {
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
                        && !tab_core::remote_mcp::sync::syncs(cfg, &source)
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
        let next = Engine::from_config(
            cfg,
            self.store.clone(),
            self.gateway.clone(),
            self.events.clone(),
        )
        .with_mcp(cfg, self.agents.remote_mcp());
        let next = match self.leases.lock().unwrap().clone() {
            Some(l) => next.with_leases(l),
            None => next,
        };
        *engine = Some(next.start());
    }

    /// Saves settings, restarts the engine with them and tells the UI.
    pub async fn apply(&self, next: TabConfig) -> anyhow::Result<TabConfig> {
        let mut current = self.config.lock().await;
        // Only when it changed, so a folder that went away later doesn't
        // block saving everything else.
        if next.agent.workdir != current.agent.workdir
            && let Some(why) = next.agent.workdir_problem()
        {
            anyhow::bail!("Work folder: {why}");
        }
        next.save(&self.config_path)?;
        let ranking_changed = next.ranking != current.ranking || next.profile != current.profile;
        *current = next.clone();
        drop(current);
        let sandbox = tab_core::sandbox::current();
        if sandbox.config() != next.sandbox {
            sandbox.set_config(next.sandbox.clone());
        }
        self.agents.set_config(next.agent.clone()).await;
        self.agents
            .remote_mcp()
            .set_servers(next.mcp_servers.clone());
        self.restart(&next).await;
        if ranking_changed && let Some(e) = self.engine.read().await.as_ref() {
            e.rerank_all()?;
        }
        let _ = self.events.send(EngineEvent::ConfigChanged);
        Ok(next)
    }

    /// Runs `f` on the engine, or fails when it isn't running.
    pub async fn with_engine<T>(
        &self,
        f: impl AsyncFnOnce(&EngineHandle) -> anyhow::Result<T>,
    ) -> Result<T, String> {
        let guard = self.engine.read().await;
        let e = guard.as_ref().ok_or("engine is not running")?;
        f(e).await.map_err(err)
    }

    /// Sets (or clears) one remote MCP tool's policy.
    pub async fn set_remote_tool(
        &self,
        id: &str,
        tool: &str,
        policy: Option<tab_core::remote_mcp::ToolPolicy>,
    ) -> anyhow::Result<()> {
        let mut next = self.config.lock().await.clone();
        let s = next
            .mcp_servers
            .get_mut(id)
            .ok_or_else(|| anyhow::anyhow!("no such connection"))?;
        match policy {
            Some(p) => s.tools.insert(tool.to_string(), p),
            None => s.tools.remove(tool),
        };
        self.apply(next).await?;
        Ok(())
    }
}

/// Where the project indexer reads files and email from.
struct KnowledgeSources {
    hub: Option<Arc<LocalHub>>,
    backend: std::sync::Weak<Backend>,
}

#[async_trait]
impl tab_core::knowledge::Sources for KnowledgeSources {
    async fn file(&self, hash: &str) -> anyhow::Result<Option<Vec<u8>>> {
        let Some(hub) = self.hub.clone() else {
            anyhow::bail!("the hub isn't running");
        };
        let hash = hash.to_string();
        tokio::task::spawn_blocking(move || hub.file_bytes(&hash)).await?
    }

    async fn email(&self) -> Option<ConnectorConfig> {
        let b = self.backend.upgrade()?;
        let cfg = b.config.lock().await;
        cfg.connectors.get("email").filter(|c| c.enabled).cloned()
    }
}

/// Lets the MCP server reach whichever engine is running now.
struct LiveEngine(std::sync::Weak<Backend>);

impl LiveEngine {
    fn backend(&self) -> anyhow::Result<Arc<Backend>> {
        self.0
            .upgrade()
            .ok_or_else(|| anyhow::anyhow!("tab is shutting down"))
    }
}

macro_rules! engine {
    ($self:ident, $e:ident => $body:expr) => {{
        let b = $self.backend()?;
        let guard = b.engine.read().await;
        let $e = guard
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("engine is not running"))?;
        $body
    }};
}

#[async_trait]
impl EngineAccess for LiveEngine {
    async fn create_task(&self, task: NewTask) -> anyhow::Result<QueueItem> {
        engine!(self, e => e.create_task(task))
    }
    async fn preview(&self, item_id: &str) -> anyhow::Result<Preview> {
        engine!(self, e => e.preview(item_id).await)
    }
    async fn connectors(&self) -> anyhow::Result<Vec<ConnectorInfo>> {
        engine!(self, e => Ok(e.connectors()))
    }
    async fn connector_config(&self, id: &str) -> anyhow::Result<Option<ConnectorConfig>> {
        Ok(self
            .backend()?
            .config
            .lock()
            .await
            .connectors
            .get(id)
            .cloned())
    }
    async fn pages_changed(&self, connector: &str, pages: &[String]) {
        let Ok(b) = self.backend() else { return };
        match b.engine.read().await.as_ref() {
            Some(e) => e.pages_changed(connector, pages),
            None => tab_core::connectors::forget_pages(connector, pages),
        }
    }
    async fn save_connector(&self, id: &str, cfg: ConnectorConfig) -> anyhow::Result<()> {
        let b = self.backend()?;
        let mut next = b.config.lock().await.clone();
        next.connectors.insert(id.to_string(), cfg);
        b.apply(next).await.map(|_| ())
    }
    async fn set_remote_tool_policy(
        &self,
        server: &str,
        tool: &str,
        policy: tab_core::remote_mcp::ToolPolicy,
    ) -> anyhow::Result<()> {
        self.backend()?
            .set_remote_tool(server, tool, Some(policy))
            .await
    }
    async fn dry_run_workflow(&self, w: Workflow, item_id: Option<String>) -> anyhow::Result<Run> {
        engine!(self, e => e.run_workflow(w, item_id.as_deref(), true).await)
    }
    async fn run_workflow_for_agent(
        &self,
        workflow_id: String,
        item_id: Option<String>,
        session: Option<String>,
    ) -> anyhow::Result<workflow::runtime::AgentRun> {
        engine!(self, e => e
            .run_workflow_for_agent(&workflow_id, item_id.as_deref(), session.as_deref())
            .await)
    }
}

/// Opens the hub, trying a few times: another tab that's just quit may
/// still hold it.
fn open_hub(path: &Path) -> anyhow::Result<Arc<LocalHub>> {
    let mut tries = 0;
    loop {
        match LocalHub::open(path) {
            Ok(h) => return Ok(h),
            Err(e) if tries < 5 => {
                tries += 1;
                tracing::warn!(error = %e, tries, "hub didn't open; trying again");
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
            Err(e) => return Err(e),
        }
    }
}
