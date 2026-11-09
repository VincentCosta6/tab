//! Commands for remote MCP connections (Connections → Linear, Notion, a
//! server by URL). See `tab_core::remote_mcp`.

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
            tools: Default::default(),
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

/// Removes the connection and forgets its sign-in.
#[tauri::command]
pub async fn mcp_remove(state: State<'_, AppState>, id: String) -> CmdResult<()> {
    let mut next = state.config.lock().await.clone();
    next.mcp_servers.remove(&id);
    state.apply(next).await.map_err(err)?;
    state.hub.remote_mcp().sign_out(&id);
    Ok(())
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
