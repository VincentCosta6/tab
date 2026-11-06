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
mod suggest;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use chrono::Utc;
use serde::Serialize;
use tokio::sync::{broadcast, mpsc, oneshot};
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
    /// Answers to AskUserQuestion, by question text.
    Answer(serde_json::Value),
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
    /// How it was installed (winget, brew, npm, native), when tab can tell.
    pub install: Option<String>,
    /// The command that updates it, for that install.
    pub upgrade: Option<String>,
    /// The newest release, when tab could look it up.
    pub latest: Option<String>,
}

/// How an agent CLI at `path` was installed, and the command that updates it.
pub fn install_method(kind: AgentKind, path: &str) -> (&'static str, &'static str) {
    // Resolve links: Homebrew's and npm's global bins can be the same folder,
    // but the real files live in Caskroom or node_modules.
    let real = std::fs::canonicalize(path)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| path.to_string());
    let p = real.replace('\\', "/").to_lowercase();
    let npm = p.contains("node_modules") || p.contains("/npm/") || p.contains("/.nvm/");
    let brew = p.contains("/homebrew/") || p.contains("/cellar/") || p.contains("/caskroom/");
    match kind {
        AgentKind::ClaudeCode if p.contains("/winget/") => {
            ("winget", "winget upgrade Anthropic.ClaudeCode")
        }
        AgentKind::ClaudeCode if npm => ("npm", "npm install -g @anthropic-ai/claude-code@latest"),
        AgentKind::ClaudeCode if brew => ("brew", "brew upgrade --cask claude-code"),
        AgentKind::ClaudeCode => ("native", "claude update"),
        AgentKind::OpenCode if npm => ("npm", "npm install -g opencode-ai@latest"),
        AgentKind::OpenCode if brew => ("brew", "brew upgrade opencode"),
        AgentKind::OpenCode => ("native", "opencode upgrade"),
    }
}

/// The newest published version of an agent CLI, from npm. Cached for six
/// hours; `None` when offline.
pub async fn latest_version(kind: AgentKind) -> Option<String> {
    use std::sync::{LazyLock, Mutex};
    use std::time::Instant;
    static CACHE: LazyLock<Mutex<std::collections::HashMap<AgentKind, (Instant, String)>>> =
        LazyLock::new(Default::default);
    if let Some((at, v)) = CACHE.lock().unwrap().get(&kind)
        && at.elapsed() < Duration::from_secs(6 * 3600)
    {
        return Some(v.clone());
    }
    let package = match kind {
        AgentKind::ClaudeCode => "@anthropic-ai/claude-code",
        AgentKind::OpenCode => "opencode-ai",
    };
    let v: serde_json::Value = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .ok()?
        .get(format!("https://registry.npmjs.org/{package}/latest"))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    let version = v["version"].as_str()?.to_string();
    CACHE
        .lock()
        .unwrap()
        .insert(kind, (Instant::now(), version.clone()));
    Some(version)
}

pub struct AgentHub {
    pub(crate) store: Arc<Store>,
    events: broadcast::Sender<EngineEvent>,
    cfg: RwLock<AgentConfig>,
    mcp: RwLock<Option<McpEndpoint>>,
    /// Per-session files (MCP config, system prompt) live here.
    dir: PathBuf,
    running: Mutex<HashMap<String, CancellationToken>>,
    /// Live turns that take more messages while they run: Claude Code's stdin,
    /// or more prompts to the same OpenCode session.
    steering: Mutex<HashMap<String, mpsc::UnboundedSender<String>>>,
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
            steering: Mutex::new(HashMap::new()),
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

    /// Where images the person attaches to chats are kept. Agents may read it.
    pub fn attachments_dir(&self) -> PathBuf {
        self.dir.join("attachments")
    }

    /// The slash commands (built-ins, custom commands and skills) the agent
    /// reported when it last started, for the composer's `/` menu.
    pub fn slash_commands(&self) -> Vec<String> {
        std::fs::read(self.dir.join("slash_commands.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub(crate) fn save_slash_commands(&self, names: &[String]) {
        if names.is_empty() || self.slash_commands() == names {
            return;
        }
        if let Ok(b) = serde_json::to_vec(names) {
            let _ = std::fs::write(self.dir.join("slash_commands.json"), b);
        }
    }

    pub(crate) fn session_dir(&self, id: &str) -> anyhow::Result<PathBuf> {
        let d = self.dir.join("sessions").join(id);
        std::fs::create_dir_all(&d)?;
        Ok(d)
    }

    pub fn workdir(&self) -> String {
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

    /// A session keyed to something other than an item (a PR under review),
    /// running in `cwd`. A new one starts if the working directory moved, since
    /// agents tie their own sessions to a directory.
    pub fn session_for_key(
        &self,
        key: &str,
        title: &str,
        cwd: &str,
        brief: &str,
    ) -> anyhow::Result<Session> {
        let session = match self.store.session_for_key(key)? {
            Some(s) if s.cwd == cwd => s,
            _ => {
                let kind = self.config().kind.ok_or_else(|| {
                    anyhow::anyhow!("no agent is set up yet; finish onboarding in Settings")
                })?;
                let s = self
                    .store
                    .session_create_keyed(key, kind, title, cwd, Utc::now())?;
                self.emit(EngineEvent::SessionChanged { session: s.clone() });
                s
            }
        };
        self.store.session_set_brief(&session.id, brief)?;
        Ok(session)
    }

    pub fn is_running(&self, session_id: &str) -> bool {
        self.running.lock().unwrap().contains_key(session_id)
    }

    /// Whether the running turn can take a message mid-flight.
    pub fn can_steer(&self, session_id: &str) -> bool {
        self.steering.lock().unwrap().contains_key(session_id)
    }

    /// Hands the person's message to the turn already running, so the agent
    /// picks it up at its next step instead of after it finishes. Returns
    /// false when no live turn takes messages (it already finished).
    pub fn steer(&self, session_id: &str, text: &str) -> anyhow::Result<bool> {
        let text = text.trim();
        anyhow::ensure!(!text.is_empty(), "message is empty");
        let tx = self.steering.lock().unwrap().get(session_id).cloned();
        let Some(tx) = tx else { return Ok(false) };
        if tx.send(text.to_string()).is_err() {
            return Ok(false);
        }
        self.post(
            session_id,
            "user",
            "text",
            text,
            serde_json::json!({ "steer": true }),
        );
        Ok(true)
    }

    pub(crate) fn steering_open(&self, session_id: &str) -> mpsc::UnboundedReceiver<String> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.steering.lock().unwrap().insert(session_id.into(), tx);
        rx
    }

    /// Stops taking messages; the turn's stdin closes once the queue drains.
    pub(crate) fn steering_close(&self, session_id: &str) {
        self.steering.lock().unwrap().remove(session_id);
    }

    /// Sends the person's message and runs one agent turn in the background.
    pub fn send(self: &Arc<Self>, session_id: &str, text: &str) -> anyhow::Result<()> {
        self.start_turn(session_id, text, true, &[])
    }

    /// Sends a message with images the person attached (paths from
    /// [`Self::attachments_dir`]). The agent is told to open them.
    pub fn send_with(
        self: &Arc<Self>,
        session_id: &str,
        text: &str,
        attachments: &[String],
    ) -> anyhow::Result<()> {
        self.start_turn(session_id, text, true, attachments)
    }

    /// Picks up turns that were cut off when tab last quit: each session
    /// still marked working resumes its agent conversation (`--resume`) with
    /// a note to carry on. One that never got as far as starting is marked
    /// failed instead. Call once at startup, after the MCP server is up.
    pub fn resume_interrupted(self: &Arc<Self>) {
        let Ok(sessions) = self.store.sessions_recent(500) else {
            return;
        };
        for s in sessions
            .into_iter()
            .filter(|s| s.status == SessionStatus::Working)
        {
            if s.agent_session_id.is_none() {
                self.post(
                    &s.id,
                    "system",
                    "note",
                    "tab quit before the agent started. Send your message again.",
                    serde_json::Value::Null,
                );
                self.update(&s.id, Some(SessionStatus::Failed), None);
                continue;
            }
            self.post(
                &s.id,
                "system",
                "note",
                "tab restarted while the agent was working, so it's picking up where it left off.",
                serde_json::Value::Null,
            );
            self.update(&s.id, Some(SessionStatus::Idle), None);
            let prompt = "tab (the app running you) restarted in the middle of your last turn, so it was cut off. \
                          Continue where you left off; redo any tool call that didn't finish.";
            match self.start_turn(&s.id, prompt, false, &[]) {
                Ok(()) => tracing::info!(session = %s.id, "resumed an interrupted agent turn"),
                Err(e) => {
                    tracing::warn!(session = %s.id, error = %e, "couldn't resume an interrupted turn")
                }
            }
        }
    }

    /// Runs one agent turn. `visible` turns show the prompt as the person's
    /// message; tab's own prompts (resuming) don't.
    fn start_turn(
        self: &Arc<Self>,
        session_id: &str,
        text: &str,
        visible: bool,
        attachments: &[String],
    ) -> anyhow::Result<()> {
        let session = self
            .store
            .session_get(session_id)?
            .ok_or_else(|| anyhow::anyhow!("no such session"))?;
        let text = text.trim();
        anyhow::ensure!(
            !text.is_empty() || !attachments.is_empty(),
            "message is empty"
        );
        let dir = self.attachments_dir();
        for a in attachments {
            anyhow::ensure!(
                Path::new(a).starts_with(&dir) && Path::new(a).is_file(),
                "that attachment is missing"
            );
        }
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
        if visible {
            let meta = if attachments.is_empty() {
                serde_json::Value::Null
            } else {
                serde_json::json!({ "attachments": attachments })
            };
            self.post(session_id, "user", "text", text, meta);
        }
        self.update(session_id, Some(SessionStatus::Working), None);

        let hub = self.clone();
        let mut prompt = text.to_string();
        if !attachments.is_empty() {
            prompt.push_str(&format!(
                "\n\nThe person attached {} (open each with your Read tool to see it):\n{}",
                if attachments.len() == 1 {
                    "an image"
                } else {
                    "images"
                },
                attachments
                    .iter()
                    .map(|a| format!("- {a}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            ));
        }
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
            hub.steering_close(&session.id);
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
                Ok(()) => suggest::spawn(hub.clone(), session.id.clone()),
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
            Decision::Answer(_) => "Answered".to_string(),
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
             - Keep this task's context tidy, so the person can navigate it: attach what is directly part of \
             the work, nothing more. Attach straight to Context with tab_attach_context (a queue item via \
             ref_item_id, or a url): the PR or branch you open for this task, the ticket or issue it \
             implements, the thread or email you're answering, the doc you're editing. Use tab_search first \
             to see whether it's already in their queue.\n\
             - Something that might matter but isn't directly part of the work (a similar old PR, a related \
             discussion) goes under Found with tab_add_found, for the person to decide. Don't attach things \
             that are only loosely connected; a few direct links beat a big web.\n\
             - To ask the person to choose between options, use AskUserQuestion: tab shows the question \
             with buttons in the chat and returns their answers.\n\
             - Linking context is a core part of your job in tab. The moment you find, open, create or \
             rely on anything that belongs to this task (a PR or branch, ticket, issue, doc, thread, \
             email, meeting, dashboard), link it: attach it with tab_attach_context right then, not at \
             the end, and don't wait to be asked. Do it for what you create too: a PR you open is \
             attached before you reply. If you're unsure it's directly part of the work, file it under \
             Found with tab_add_found instead. Check tab_search first so you don't attach duplicates.\n\
             - Whenever you mention one of these in a reply, write its full URL or a markdown link \
             `[label](url)`, never just a name or number. tab turns links to connected services (GitHub, \
             Jira, Slack, ...) into rich pills with a hover preview, and only a real URL gets that. If you \
             only have a reference such as `owner/repo#12`, build or look up its URL first.\n\
             - If the work splits into follow-ups, create them with tab_create_task.\n\
             - tab_search matches by keyword and by meaning. Give it a window (within_days, or since/until) \
             and project: \"current\" when the person means recent or this project's work; it widens on its \
             own if the window is too narrow.\n\
             - When the person mentions something you don't recognise (a project, a codename, a person, a \
             repo), call tab_graph_search before asking them: it searches everything tab knows and shows \
             what's connected to it. tab_graph_expand follows one connection further. tab_project shows a \
             whole project: its notes, what it's about and its recent work.\n\
             - When you learn a durable fact about the project (how it deploys, who owns what, where things \
             live, a decision and why), save it with tab_project_note under a short topic, citing the items \
             it came from. Writing the same topic again replaces the note, so keep it current rather than \
             adding new ones. Skip anything that's only true for this one task, and never note one-off \
             events (sign-ins, verifications, receipts, codes), credentials or personal account details: \
             every future chat reads these.\n\
             - When the person asks to link, file or move something into a project, use tab_set_project; \
             a note is not a link.\n\
             - When the person wants to connect a service (email, GitHub, ...), set it up for them: \
             tab_list_connections shows each connection's settings. For email, call tab_find_email_server \
             with their address to find the IMAP server instead of asking them for it. Save settings with \
             tab_configure_connection. Never ask for passwords or tokens in chat text: tab shows a card the \
             person types them into (tab_ask_for_secret shows it again). Once they say it's entered, run \
             tab_test_connection and explain any failure plainly, e.g. that their host needs an app password.\n\
             Keep replies short; the person reads them in a side panel.\n",
        );
        if let Ok(Some(brief)) = self.store.session_brief(&session.id) {
            out.push('\n');
            out.push_str(&brief);
            out.push('\n');
        }
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
        if let Some(p) = ctx
            .item
            .project_id
            .as_deref()
            .and_then(|p| self.store.project_find(p).ok().flatten())
        {
            out.push_str(&format!(
                "Project: {} (call tab_project for its recent work)\n",
                p.name
            ));
            let notes = self.store.project_notes(&p.id).unwrap_or_default();
            if !notes.is_empty() {
                out.push_str("Project notes, from earlier chats:\n");
                for n in notes.iter().take(20) {
                    out.push_str(&format!(
                        "- {} ({}, {}): {}\n",
                        n.topic,
                        n.added_by,
                        n.updated_at.format("%Y-%m-%d"),
                        n.text.chars().take(500).collect::<String>()
                    ));
                }
            }
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

    /// The configured agent: where it is, its version, the newest release
    /// and how to update it.
    pub async fn health(&self) -> Option<AgentProbe> {
        let cfg = self.config();
        let kind = cfg.kind?;
        let mut p = probe(kind, cfg.command.as_deref()).await;
        if p.found {
            p.latest = latest_version(kind).await;
        }
        Some(p)
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
        install: None,
        upgrade: None,
        latest: None,
    };
    let exe = match resolve(name) {
        Ok(e) => e,
        Err(e) => {
            p.error = Some(e.to_string());
            return p;
        }
    };
    p.path = Some(exe.display().to_string());
    let (install, upgrade) = install_method(kind, &exe.display().to_string());
    p.install = Some(install.into());
    p.upgrade = Some(upgrade.into());
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

#[cfg(test)]
mod keyed_tests {
    use super::*;

    #[test]
    fn keyed_sessions_are_reused_per_directory_and_carry_a_brief() {
        let store = Arc::new(Store::in_memory().unwrap());
        let (tx, _rx) = broadcast::channel(16);
        let cfg = AgentConfig {
            kind: Some(AgentKind::ClaudeCode),
            ..Default::default()
        };
        let hub = AgentHub::new(store.clone(), tx, cfg, std::env::temp_dir());
        let a = hub
            .session_for_key("review:o/r#1", "PR", "/w/pr-1", "About PR 1")
            .unwrap();
        let b = hub
            .session_for_key("review:o/r#1", "PR", "/w/pr-1", "About PR 1, updated")
            .unwrap();
        assert_eq!(a.id, b.id);
        assert!(hub.system_prompt(&b).contains("About PR 1, updated"));
        let c = hub
            .session_for_key("review:o/r#1", "PR", "/elsewhere", "x")
            .unwrap();
        assert_ne!(a.id, c.id);
        assert_ne!(
            hub.session_for_key("review:o/r#2", "PR", "/w/pr-2", "y")
                .unwrap()
                .id,
            c.id
        );
    }

    #[tokio::test]
    async fn a_turn_cut_off_before_it_started_is_marked_failed() {
        let store = Arc::new(Store::in_memory().unwrap());
        let (tx, _rx) = broadcast::channel(16);
        let hub = AgentHub::new(
            store.clone(),
            tx,
            AgentConfig::default(),
            std::env::temp_dir(),
        );
        let s = store
            .session_create(None, AgentKind::ClaudeCode, "t", ".", Utc::now())
            .unwrap();
        store
            .session_update(&s.id, Some(SessionStatus::Working), None, Utc::now())
            .unwrap();
        hub.resume_interrupted();
        assert_eq!(
            store.session_get(&s.id).unwrap().unwrap().status,
            SessionStatus::Failed
        );
        assert!(
            store
                .messages(&s.id)
                .unwrap()
                .iter()
                .any(|m| m.content.contains("tab quit"))
        );
    }
}

#[cfg(test)]
mod install_tests {
    use super::*;

    #[test]
    fn update_command_follows_how_claude_code_was_installed() {
        let cmd = |p| install_method(AgentKind::ClaudeCode, p).1;
        assert_eq!(
            cmd(
                r"C:\Users\c\AppData\Local\Microsoft\WinGet\Packages\Anthropic.ClaudeCode_x\claude.exe"
            ),
            "winget upgrade Anthropic.ClaudeCode"
        );
        assert_eq!(
            cmd("/opt/homebrew/bin/claude"),
            "brew upgrade --cask claude-code"
        );
        assert_eq!(
            cmd("/Users/v/.nvm/versions/node/v22/bin/claude"),
            "npm install -g @anthropic-ai/claude-code@latest"
        );
        assert_eq!(cmd("/Users/v/.local/bin/claude"), "claude update");
    }
}
