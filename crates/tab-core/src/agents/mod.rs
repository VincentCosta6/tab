//! Agent sessions: tab drives a coding agent (Claude Code or OpenCode) on
//! behalf of a queue item and streams the conversation into the chat view.
//!
//! * One session per item. Chatting on an item reuses its latest session;
//!   the agent's own session id is kept so each turn resumes it.
//! * Every message is stored (`session_messages`) and published as an
//!   [`EngineEvent`], with streaming text as `SessionDelta`s.
//! * When the agent needs permission for a tool, it asks tab: Claude Code
//!   through the `approve` tool on tab's MCP server (`--permission-prompt-tool`),
//!   OpenCode through its `permission.asked` event. Either way the person
//!   answers in the chat. Nothing runs with permissions bypassed.
//! * Agents reach back into tab through the MCP server ([`crate::mcp`]) to
//!   search the queue, attach context, file what they find and create tasks.

mod claude;
mod opencode;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use chrono::Utc;
use serde::Serialize;
use tokio::sync::{broadcast, oneshot};
use tokio_util::sync::CancellationToken;

use crate::config::AgentConfig;
use crate::engine::EngineEvent;
use crate::model::{AgentKind, Approval, ContextSection, Session, SessionStatus};
use crate::store::Store;

/// How long an approval waits for the person before it is denied.
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(30 * 60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allow,
    AllowAlways,
    Deny(String),
}

/// Where agents reach tab's MCP server.
#[derive(Debug, Clone)]
pub struct McpEndpoint {
    pub url: String,
    pub token: String,
}

/// Result of looking for an agent CLI, for onboarding.
#[derive(Debug, Clone, Serialize)]
pub struct AgentProbe {
    pub kind: AgentKind,
    pub found: bool,
    pub path: Option<String>,
    pub version: Option<String>,
    pub error: Option<String>,
}

pub struct AgentHub {
    pub(crate) store: Arc<Store>,
    events: broadcast::Sender<EngineEvent>,
    cfg: RwLock<AgentConfig>,
    mcp: RwLock<Option<McpEndpoint>>,
    /// Per-session files (MCP config, system prompt) live here.
    dir: PathBuf,
    running: Mutex<HashMap<String, CancellationToken>>,
    approvals: Mutex<HashMap<String, (Approval, oneshot::Sender<Decision>)>>,
    opencode: tokio::sync::Mutex<Option<Arc<opencode::Server>>>,
}

impl AgentHub {
    pub fn new(
        store: Arc<Store>,
        events: broadcast::Sender<EngineEvent>,
        cfg: AgentConfig,
        dir: PathBuf,
    ) -> Arc<Self> {
        Arc::new(Self {
            store,
            events,
            cfg: RwLock::new(cfg),
            mcp: RwLock::new(None),
            dir,
            running: Mutex::new(HashMap::new()),
            approvals: Mutex::new(HashMap::new()),
            opencode: tokio::sync::Mutex::new(None),
        })
    }

    pub async fn set_config(&self, cfg: AgentConfig) {
        let changed = *self.cfg.read().unwrap() != cfg;
        *self.cfg.write().unwrap() = cfg;
        if changed && let Some(old) = self.opencode.lock().await.take() {
            old.shutdown();
        }
    }

    pub fn config(&self) -> AgentConfig {
        self.cfg.read().unwrap().clone()
    }

    pub fn set_mcp(&self, endpoint: McpEndpoint) {
        *self.mcp.write().unwrap() = Some(endpoint);
    }

    pub(crate) fn mcp(&self) -> Option<McpEndpoint> {
        self.mcp.read().unwrap().clone()
    }

    pub(crate) fn emit(&self, e: EngineEvent) {
        let _ = self.events.send(e);
    }

    pub(crate) fn session_dir(&self, id: &str) -> anyhow::Result<PathBuf> {
        let d = self.dir.join("sessions").join(id);
        std::fs::create_dir_all(&d)?;
        Ok(d)
    }

    pub(crate) fn workdir(&self) -> String {
        self.config()
            .workdir
            .filter(|w| !w.trim().is_empty())
            .or_else(|| {
                std::env::var("USERPROFILE")
                    .or_else(|_| std::env::var("HOME"))
                    .ok()
            })
            .unwrap_or_else(|| ".".into())
    }

    /// Stores and publishes a transcript entry.
    pub(crate) fn post(
        &self,
        session_id: &str,
        role: &str,
        kind: &str,
        content: &str,
        meta: serde_json::Value,
    ) {
        match self
            .store
            .message_add(session_id, role, kind, content, &meta, Utc::now())
        {
            Ok(message) => self.emit(EngineEvent::SessionMessage { message }),
            Err(e) => tracing::error!(error = %e, "storing session message failed"),
        }
    }

    pub(crate) fn delta(&self, session_id: &str, text: &str) {
        self.emit(EngineEvent::SessionDelta {
            session_id: session_id.into(),
            text: text.into(),
        });
    }

    pub(crate) fn update(
        &self,
        session_id: &str,
        status: Option<SessionStatus>,
        agent_session_id: Option<&str>,
    ) {
        match self
            .store
            .session_update(session_id, status, agent_session_id, Utc::now())
        {
            Ok(Some(session)) => self.emit(EngineEvent::SessionChanged { session }),
            Ok(None) => {}
            Err(e) => tracing::error!(error = %e, "updating session failed"),
        }
    }

    /// The session for an item, created on first use.
    pub fn session_for_item(&self, item_id: &str) -> anyhow::Result<Session> {
        if let Some(s) = self.store.session_for_item(item_id)? {
            return Ok(s);
        }
        let item = self
            .store
            .get(item_id)?
            .ok_or_else(|| anyhow::anyhow!("no such item"))?;
        let kind = self.config().kind.ok_or_else(|| {
            anyhow::anyhow!("no agent is set up yet; finish onboarding in Settings")
        })?;
        let session = self.store.session_create(
            Some(item_id),
            kind,
            &item.event.title,
            &self.workdir(),
            Utc::now(),
        )?;
        self.emit(EngineEvent::SessionChanged {
            session: session.clone(),
        });
        Ok(session)
    }

    pub fn is_running(&self, session_id: &str) -> bool {
        self.running.lock().unwrap().contains_key(session_id)
    }

    /// Sends the person's message and runs one agent turn in the background.
    pub fn send(self: &Arc<Self>, session_id: &str, text: &str) -> anyhow::Result<()> {
        let session = self
            .store
            .session_get(session_id)?
            .ok_or_else(|| anyhow::anyhow!("no such session"))?;
        let text = text.trim();
        anyhow::ensure!(!text.is_empty(), "message is empty");
        if let Some(why) = self
            .config()
            .budget
            .blocked(&self.store.usage_summary(Utc::now())?)
        {
            anyhow::bail!(why);
        }
        let cancel = CancellationToken::new();
        {
            let mut running = self.running.lock().unwrap();
            anyhow::ensure!(
                !running.contains_key(session_id),
                "the agent is still working on the last message"
            );
            running.insert(session_id.into(), cancel.clone());
        }
        self.post(session_id, "user", "text", text, serde_json::Value::Null);
        self.update(session_id, Some(SessionStatus::Working), None);

        let hub = self.clone();
        let prompt = text.to_string();
        tokio::spawn(async move {
            let system = hub.system_prompt(&session);
            let result = match session.agent {
                AgentKind::ClaudeCode => {
                    claude::run_turn(&hub, &session, &prompt, &system, cancel.clone()).await
                }
                AgentKind::OpenCode => {
                    opencode::run_turn(&hub, &session, &prompt, &system, cancel.clone()).await
                }
            };
            hub.running.lock().unwrap().remove(&session.id);
            // Anything still waiting on the person is moot now.
            hub.drop_approvals(&session.id);
            match result {
                Ok(()) if cancel.is_cancelled() => {
                    hub.post(
                        &session.id,
                        "system",
                        "status",
                        "Stopped",
                        serde_json::Value::Null,
                    );
                    hub.update(&session.id, Some(SessionStatus::Idle), None);
                }
                Ok(()) => {}
                Err(e) => {
                    tracing::warn!(session = %session.id, error = %e, "agent turn failed");
                    hub.post(
                        &session.id,
                        "system",
                        "error",
                        &format!("{e:#}"),
                        serde_json::Value::Null,
                    );
                    hub.update(&session.id, Some(SessionStatus::Failed), None);
                }
            }
        });
        Ok(())
    }

    pub fn stop(&self, session_id: &str) {
        if let Some(c) = self.running.lock().unwrap().get(session_id) {
            c.cancel();
        }
    }

    /// Asks the person to approve a tool call and waits for the answer.
    pub async fn request_approval(
        &self,
        session_id: &str,
        tool: &str,
        summary: String,
        input: serde_json::Value,
    ) -> Decision {
        let approval = Approval {
            id: uuid::Uuid::new_v4().to_string(),
            session_id: session_id.into(),
            tool: tool.into(),
            summary,
            input,
        };
        let (tx, rx) = oneshot::channel();
        self.approvals
            .lock()
            .unwrap()
            .insert(approval.id.clone(), (approval.clone(), tx));
        self.update(session_id, Some(SessionStatus::NeedsInput), None);
        self.emit(EngineEvent::ApprovalRequested {
            approval: approval.clone(),
        });
        let decision = match tokio::time::timeout(APPROVAL_TIMEOUT, rx).await {
            Ok(Ok(d)) => d,
            Ok(Err(_)) => Decision::Deny("The session ended before the person answered.".into()),
            Err(_) => {
                self.approvals.lock().unwrap().remove(&approval.id);
                Decision::Deny("Nobody answered in time.".into())
            }
        };
        self.emit(EngineEvent::ApprovalResolved {
            session_id: session_id.into(),
            id: approval.id.clone(),
        });
        let verdict = match &decision {
            Decision::Allow => "Allowed".to_string(),
            Decision::AllowAlways => "Always allowed".to_string(),
            Decision::Deny(why) => format!("Denied: {why}"),
        };
        self.post(
            session_id,
            "system",
            "approval",
            &format!("{verdict} · {}", approval.summary),
            serde_json::json!({ "tool": approval.tool }),
        );
        if self.is_running(session_id) {
            self.update(session_id, Some(SessionStatus::Working), None);
        }
        decision
    }

    /// The person's answer to an approval. Returns false if it was no longer pending.
    pub fn resolve_approval(&self, id: &str, decision: Decision) -> bool {
        match self.approvals.lock().unwrap().remove(id) {
            Some((_, tx)) => tx.send(decision).is_ok(),
            None => false,
        }
    }

    pub fn pending_approvals(&self, session_id: &str) -> Vec<Approval> {
        self.approvals
            .lock()
            .unwrap()
            .values()
            .filter(|(a, _)| a.session_id == session_id)
            .map(|(a, _)| a.clone())
            .collect()
    }

    fn drop_approvals(&self, session_id: &str) {
        self.approvals
            .lock()
            .unwrap()
            .retain(|_, (a, _)| a.session_id != session_id);
    }

    /// Instructions appended to the agent's system prompt on every turn, so
    /// it always sees the item and its current context.
    pub(crate) fn system_prompt(&self, session: &Session) -> String {
        let mut out = String::from(
            "You are working inside tab, the person's work queue. tab's MCP server (tools named tab_*) \
             lets you search their queue, read items, attach context and create tasks.\n\
             - When you find something relevant (a PR, ticket, thread, doc), call tab_add_found so it shows \
             under Found for the person to promote.\n\
             - When the person asks you to work on a specific ticket, PR or thread from their queue, call \
             tab_search to find it and tab_attach_context to attach it to this task.\n\
             - If the work splits into follow-ups, create them with tab_create_task.\n\
             Keep replies short; the person reads them in a side panel.\n",
        );
        let Some(item_id) = &session.item_id else {
            return out;
        };
        let Ok(Some(ctx)) = self.store.item_context(item_id) else {
            return out;
        };
        let e = &ctx.item.event;
        out.push_str(&format!(
            "\nThis session belongs to tab item {} ({}): {}\n",
            ctx.item.id, e.kind, e.title
        ));
        if let Some(a) = &e.actor {
            out.push_str(&format!("From: {a}\n"));
        }
        if let Some(c) = &e.context {
            out.push_str(&format!("Where: {c}\n"));
        }
        if let Some(u) = &e.url {
            out.push_str(&format!("Link: {u}\n"));
        }
        if let Some(b) = &e.body {
            out.push_str(&format!(
                "Details:\n{}\n",
                b.chars().take(4000).collect::<String>()
            ));
        }
        let context: Vec<_> = ctx
            .entries
            .iter()
            .filter(|c| c.section == ContextSection::Context)
            .collect();
        if !context.is_empty() {
            out.push_str("\nContext the person attached:\n");
            for c in context {
                let pin = if c.pinned { " (pinned)" } else { "" };
                let link = c
                    .url
                    .as_deref()
                    .or(c.ref_item.as_ref().and_then(|i| i.event.url.as_deref()))
                    .unwrap_or("");
                out.push_str(&format!("- {}{pin} {link}\n", c.title));
                if let Some(s) = &c.summary {
                    out.push_str(&format!("  {}\n", s.chars().take(400).collect::<String>()));
                }
                if let Some(i) = &c.ref_item {
                    out.push_str(&format!("  tab item {}\n", i.id));
                }
            }
        }
        out
    }

    pub fn shutdown(&self) {
        for c in self.running.lock().unwrap().values() {
            c.cancel();
        }
        if let Ok(mut oc) = self.opencode.try_lock()
            && let Some(s) = oc.take()
        {
            s.shutdown();
        }
    }

    /// Looks for both supported agents, for onboarding.
    pub async fn detect(&self) -> Vec<AgentProbe> {
        let cfg = self.config();
        let mut out = Vec::new();
        for kind in [AgentKind::OpenCode, AgentKind::ClaudeCode] {
            let custom = (cfg.kind == Some(kind))
                .then(|| cfg.command.clone())
                .flatten();
            out.push(probe(kind, custom.as_deref()).await);
        }
        out
    }
}

pub(crate) fn default_command(kind: AgentKind) -> &'static str {
    match kind {
        AgentKind::ClaudeCode => "claude",
        AgentKind::OpenCode => "opencode",
    }
}

/// Finds an executable on PATH (including `.cmd` shims on Windows).
pub fn resolve(command: &str) -> anyhow::Result<PathBuf> {
    let p = PathBuf::from(command);
    if p.components().count() > 1 && p.exists() {
        return Ok(p);
    }
    which::which(command).map_err(|_| {
        anyhow::anyhow!("`{command}` was not found on PATH. Install it or set its full path in Settings → Agent.")
    })
}

/// A command that never opens a console window on Windows.
pub fn command(exe: &std::path::Path) -> tokio::process::Command {
    #[allow(unused_mut)]
    let mut cmd = tokio::process::Command::new(exe);
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd.kill_on_drop(true);
    cmd
}

async fn probe(kind: AgentKind, custom: Option<&str>) -> AgentProbe {
    let name = custom.unwrap_or(default_command(kind));
    let mut p = AgentProbe {
        kind,
        found: false,
        path: None,
        version: None,
        error: None,
    };
    let exe = match resolve(name) {
        Ok(e) => e,
        Err(e) => {
            p.error = Some(e.to_string());
            return p;
        }
    };
    p.path = Some(exe.display().to_string());
    let out = tokio::time::timeout(
        Duration::from_secs(15),
        command(&exe).arg("--version").output(),
    )
    .await;
    match out {
        Ok(Ok(o)) if o.status.success() => {
            p.found = true;
            p.version = Some(String::from_utf8_lossy(&o.stdout).trim().to_string());
        }
        Ok(Ok(o)) => p.error = Some(String::from_utf8_lossy(&o.stderr).trim().to_string()),
        Ok(Err(e)) => p.error = Some(e.to_string()),
        Err(_) => p.error = Some("timed out asking for its version".into()),
    }
    p
}

/// Short, human summary of a tool call for an approval card.
pub(crate) fn summarize_tool(tool: &str, input: &serde_json::Value) -> String {
    let s = |k: &str| input.get(k).and_then(|v| v.as_str());
    let detail = s("command")
        .or(s("file_path"))
        .or(s("filePath"))
        .or(s("path"))
        .or(s("url"))
        .or(s("pattern"))
        .or(s("description"));
    match detail {
        Some(d) => format!("{tool}: {}", d.chars().take(300).collect::<String>()),
        None => tool.to_string(),
    }
}
