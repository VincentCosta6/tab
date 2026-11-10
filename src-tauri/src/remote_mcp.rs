//! Commands for remote MCP connections (Connections → Linear, Notion, a
//! server by URL). See `tab_core::remote_mcp`.

use std::path::PathBuf;

use tab_core::remote_mcp::import::{self, Candidate};
use tab_core::remote_mcp::{
    self, CatalogEntry, Probe, RemoteMcpConfig, RemoteMcpInfo, ToolPolicy, ToolView,
};
use tauri::{Emitter, State};

use crate::{AppState, CmdResult, err};

/// Services tab knows the MCP server of.
#[tauri::command]
pub async fn mcp_catalog() -> Vec<CatalogEntry> {
    remote_mcp::catalog()
}

/// Every connected server and whether it's ready.
#[tauri::command]
pub async fn mcp_servers(state: State<'_, AppState>) -> CmdResult<Vec<RemoteMcpInfo>> {
    Ok(state.hub.remote_mcp().infos())
}

/// What's at `url`: an MCP server? Does it want a sign-in?
#[tauri::command]
pub async fn mcp_probe(state: State<'_, AppState>, url: String) -> CmdResult<Probe> {
    let taken: Vec<String> = state
        .config
        .lock()
        .await
        .mcp_servers
        .keys()
        .cloned()
        .collect();
    state
        .hub
        .remote_mcp()
        .probe(&url, &taken)
        .await
        .map_err(err)
}

/// Adds a server, then starts its sign-in when it wants one. Returns the
/// page to open in the browser, if any (the UI opens it).
#[tauri::command]
pub async fn mcp_add(
    state: State<'_, AppState>,
    id: String,
    name: String,
    url: String,
    sign_in: bool,
    catalog: Option<String>,
) -> CmdResult<Option<String>> {
    if !remote_mcp::valid_id(&id) {
        return Err(
            "Pick a short lowercase name (letters, numbers and dashes), not \"tab\".".into(),
        );
    }
    let url = remote_mcp::check_url(&url).map_err(err)?.to_string();
    let mut next = state.config.lock().await.clone();
    if next.mcp_servers.contains_key(&id) {
        return Err(format!("There's already a connection called {id}."));
    }
    next.mcp_servers.insert(
        id.clone(),
        RemoteMcpConfig {
            name: name.trim().to_string(),
            url,
            enabled: true,
            sign_in,
            catalog,
            ..Default::default()
        },
    );
    state.apply(next).await.map_err(err)?;
    if sign_in {
        Ok(Some(
            state
                .hub
                .remote_mcp()
                .begin_sign_in(&id)
                .await
                .map_err(err)?,
        ))
    } else {
        Ok(None)
    }
}

/// "Sign in with Google / Microsoft" for the email connection: which
/// providers this build offers, and who's signed in.
#[derive(serde::Serialize)]
pub struct EmailSignInStatus {
    providers: Vec<tab_core::mail_oauth::ProviderInfo>,
    signed_in: Option<tab_core::mail_oauth::SignedIn>,
}

#[tauri::command]
pub async fn email_sign_in_status() -> EmailSignInStatus {
    EmailSignInStatus {
        providers: tab_core::mail_oauth::providers(),
        signed_in: tab_core::mail_oauth::signed_in(),
    }
}

/// Starts signing in to mail with `provider`. Returns the page to open;
/// the browser comes back to tab, which sets up the email connection.
#[tauri::command]
pub async fn email_sign_in(state: State<'_, AppState>, provider: String) -> CmdResult<String> {
    let p = tab_core::mail_oauth::Provider::parse(&provider).ok_or("unknown provider")?;
    let base = state
        .hub
        .remote_mcp()
        .loopback()
        .ok_or("tab's MCP server isn't running, so the sign-in has nowhere to come back to")?;
    tab_core::mail_oauth::begin(p, &base).map_err(err)
}

/// Forgets the email sign-in and turns the connection off until it signs
/// in again or gets a password.
#[tauri::command]
pub async fn email_sign_out(state: State<'_, AppState>) -> CmdResult<()> {
    tab_core::mail_oauth::sign_out();
    let mut next = state.config.lock().await.clone();
    if let Some(c) = next.connectors.get_mut("email")
        && c.str("auth").is_some()
    {
        c.settings.remove("auth");
        c.enabled = false;
        state.apply(next).await.map_err(err)?;
    }
    let _ = state.events.send(tab_core::EngineEvent::ConfigChanged);
    Ok(())
}

/// Starts (or restarts) a sign-in. Returns the page to open.
#[tauri::command]
pub async fn mcp_sign_in(state: State<'_, AppState>, id: String) -> CmdResult<String> {
    state.hub.remote_mcp().begin_sign_in(&id).await.map_err(err)
}

#[tauri::command]
pub async fn mcp_sign_out(state: State<'_, AppState>, id: String) -> CmdResult<()> {
    state.hub.remote_mcp().sign_out(&id);
    let _ = state.events.send(tab_core::EngineEvent::ConfigChanged);
    Ok(())
}

/// Removes the connection and forgets its sign-in and keys.
#[tauri::command]
pub async fn mcp_remove(state: State<'_, AppState>, id: String) -> CmdResult<()> {
    let mut next = state.config.lock().await.clone();
    next.mcp_servers.remove(&id);
    state.apply(next).await.map_err(err)?;
    state.hub.remote_mcp().forget(&id);
    Ok(())
}

fn home() -> Option<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from)
}

/// The MCP servers in Claude Code (`~/.claude.json`, `.mcp.json` in known
/// projects, claude.ai connectors), with what tab already has marked.
async fn scan(core: &crate::Core) -> Vec<Candidate> {
    let mut repos = Vec::new();
    for p in core.store.projects().unwrap_or_default() {
        for r in core.store.project_repos(&p.id).unwrap_or_default() {
            repos.extend(r.path.map(PathBuf::from));
        }
    }
    let existing = core.config.lock().await.mcp_servers.clone();
    let claude_ai = core.hub.claude_ai_connectors();
    let remote = core.hub.remote_mcp().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let home = home();
        let src = import::read_sources(home.as_deref(), &repos, claude_ai);
        import::scan(
            &src,
            &existing,
            |id| remote.secrets(id),
            home.as_deref(),
            |k| std::env::var(k).ok(),
        )
    })
    .await
    .unwrap_or_default()
}

#[tauri::command]
pub async fn mcp_import_scan(state: State<'_, AppState>) -> CmdResult<Vec<Candidate>> {
    Ok(scan(&state).await)
}

/// How one server's import went.
#[derive(serde::Serialize)]
pub struct ImportResult {
    pub key: String,
    pub id: String,
    pub name: String,
    /// It signs in with OAuth, so the person signs in again in tab.
    pub sign_in: bool,
    pub error: Option<String>,
}

/// Turns the chosen Claude Code servers (by `key`) into tab connections.
/// Copies only: nothing in Claude Code changes. Keys go to the vault.
#[tauri::command]
pub async fn mcp_import(
    state: State<'_, AppState>,
    keys: Vec<String>,
) -> CmdResult<Vec<ImportResult>> {
    let found = scan(&state).await;
    let remote = state.hub.remote_mcp().clone();
    let mut next = state.config.lock().await.clone();
    let mut out = Vec::new();
    for key in keys {
        let Some(c) = found.iter().find(|c| c.key == key && c.actionable()) else {
            out.push(ImportResult {
                key,
                id: String::new(),
                name: String::new(),
                sign_in: false,
                error: Some("It's no longer in Claude Code, or tab already has it.".into()),
            });
            continue;
        };
        let Some(mut cfg) = c.config.clone() else {
            continue;
        };
        let fail = |e: String| ImportResult {
            key: c.key.clone(),
            id: c.id.clone(),
            name: c.name.clone(),
            sign_in: false,
            error: Some(e),
        };
        // A plain URL: does it want a sign-in?
        if c.kind == "remote" {
            match remote.probe(&cfg.url, &[]).await {
                Ok(p) => cfg.sign_in = p.sign_in,
                Err(e) => {
                    out.push(fail(e.to_string()));
                    continue;
                }
            }
        }
        // Importing again keeps what the person chose in tab.
        if let Some(old) = c.existing.as_ref().and_then(|id| next.mcp_servers.get(id)) {
            cfg.enabled = old.enabled;
            cfg.tools = old.tools.clone();
            cfg.catalog = cfg.catalog.or_else(|| old.catalog.clone());
        }
        if let Err(e) = remote.set_secrets(&c.id, &c.secrets) {
            out.push(fail(format!("Couldn't keep its keys: {e}")));
            continue;
        }
        out.push(ImportResult {
            key: c.key.clone(),
            id: c.id.clone(),
            name: cfg.name.clone(),
            sign_in: cfg.sign_in,
            error: None,
        });
        next.mcp_servers.insert(c.id.clone(), cfg);
    }
    state.apply(next).await.map_err(err)?;
    Ok(out)
}

#[tauri::command]
pub async fn mcp_set_enabled(
    state: State<'_, AppState>,
    id: String,
    enabled: bool,
) -> CmdResult<()> {
    let mut next = state.config.lock().await.clone();
    let s = next.mcp_servers.get_mut(&id).ok_or("no such connection")?;
    s.enabled = enabled;
    state.apply(next).await.map_err(err)?;
    Ok(())
}

/// The server's tools with the person's choices. `refresh` asks the
/// server again; otherwise it's what tab saw last (asking when it has
/// never looked).
#[tauri::command]
pub async fn mcp_tools(
    state: State<'_, AppState>,
    id: String,
    refresh: bool,
) -> CmdResult<Vec<ToolView>> {
    let remote = state.hub.remote_mcp();
    let seen = remote.tool_views(&id);
    if !refresh && !seen.is_empty() {
        return Ok(seen);
    }
    let result = remote.list_tools(&id).await.map_err(err);
    let _ = state.events.send(tab_core::EngineEvent::ConfigChanged);
    result
}

/// Sets a tool to allowed, ask or off; `None` goes back to the default.
#[tauri::command]
pub async fn mcp_set_tool(
    state: State<'_, AppState>,
    id: String,
    tool: String,
    policy: Option<ToolPolicy>,
) -> CmdResult<()> {
    set_tool(&state, &id, &tool, policy).await.map_err(err)
}

pub(crate) async fn set_tool(
    core: &crate::Core,
    id: &str,
    tool: &str,
    policy: Option<ToolPolicy>,
) -> anyhow::Result<()> {
    let mut next = core.config.lock().await.clone();
    let s = next
        .mcp_servers
        .get_mut(id)
        .ok_or_else(|| anyhow::anyhow!("no such connection"))?;
    match policy {
        Some(p) => s.tools.insert(tool.to_string(), p),
        None => s.tools.remove(tool),
    };
    core.apply(next).await?;
    Ok(())
}

/// Forwards finished sign-ins to the UI as `tab://mcp-sign-in`.
pub fn forward_sign_ins(app: tauri::AppHandle, remote: std::sync::Arc<remote_mcp::RemoteMcp>) {
    let mut rx = remote.subscribe();
    tauri::async_runtime::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(e) => {
                    let _ = app.emit("tab://mcp-sign-in", &e);
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            }
        }
    });
}
