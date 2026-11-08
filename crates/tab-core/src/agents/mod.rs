//! Agent sessions: tab drives a coding agent (Claude Code, OpenCode, Codex
//! or Pi) on
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

pub mod catalog;
mod claude;
mod codex;
mod opencode;
mod per_turn;
mod pi;
mod small;
mod suggest;
mod titler;

pub use titler::DEFAULT_RULES as TITLER_DEFAULT_RULES;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Serialize;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::NewContext;
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
    /// When it isn't installed: what tab's Install button runs.
    pub install_command: Option<String>,
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
    let u = &catalog::harness(kind).upgrade;
    let winget = p.contains("/winget/");
    let pick = |name: &'static str, cmd: &'static Option<String>| cmd.as_deref().map(|c| (name, c));
    (if winget {
        pick("winget", &u.winget)
    } else {
        None
    })
    .or(if npm { pick("npm", &u.npm) } else { None })
    .or(if brew { pick("brew", &u.brew) } else { None })
    .or(pick("native", &u.native))
    .unwrap_or(("unknown", ""))
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
    let package = catalog::harness(kind).install.npm.as_deref()?;
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

/// Drops ANSI escape sequences from installer output.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.next_if_eq(&'[').is_some() {
                for n in chars.by_ref() {
                    if n.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Runs the update command for the agent CLI at `path`, the same one tab shows
/// in [`AgentProbe::upgrade`], and passes each line it prints to `on_line`.
/// Installers redraw spinners with `\r`, so `\r` ends a line too.
pub async fn run_upgrade(
    kind: AgentKind,
    path: &str,
    on_line: impl Fn(String) + Send + Sync + 'static,
) -> anyhow::Result<()> {
    let (_, cmdline) = install_method(kind, path);
    anyhow::ensure!(
        !cmdline.is_empty(),
        "tab doesn't know how to update this install."
    );
    let mut words = cmdline.split_whitespace();
    let program = words.next().unwrap_or_default();
    // The agent updating itself runs the binary we found, not whatever PATH has first.
    let exe = if program == default_command(kind) {
        PathBuf::from(path)
    } else {
        which::which(program).map_err(|_| {
            anyhow::anyhow!("`{program}` isn't on PATH, so tab can't run `{cmdline}` for you.")
        })?
    };
    let mut cmd = command(&exe);
    cmd.args(words);
    if program == "winget" {
        cmd.args(["--accept-source-agreements", "--accept-package-agreements"]);
    }
    run_streaming(cmd, cmdline, on_line).await
}

/// Installs `kind` the way its metadata says (npm when it's on PATH, else
/// its installer script), passing each line it prints to `on_line`.
pub async fn run_install(
    kind: AgentKind,
    on_line: impl Fn(String) + Send + Sync + 'static,
) -> anyhow::Result<()> {
    let h = catalog::harness(kind);
    let cmdline = h.install_command().ok_or_else(|| {
        anyhow::anyhow!("tab can't install {} on this computer by itself. Install Node.js (for npm), then try again.", h.name)
    })?;
    let cmd = if let Some(args) = cmdline.strip_prefix("npm ") {
        let npm = which::which("npm").map_err(|_| anyhow::anyhow!("`npm` isn't on PATH."))?;
        let mut c = command(&npm);
        c.args(args.split_whitespace());
        c
    } else if cfg!(windows) {
        let mut c = command(Path::new("powershell"));
        c.args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &cmdline,
        ]);
        c
    } else {
        let mut c = command(Path::new("sh"));
        c.args(["-c", &cmdline]);
        c
    };
    run_streaming(cmd, &cmdline, on_line).await
}

/// Runs an installer or updater, a line of its output at a time.
async fn run_streaming(
    mut cmd: tokio::process::Command,
    cmdline: &str,
    on_line: impl Fn(String) + Send + Sync + 'static,
) -> anyhow::Result<()> {
    use tokio::io::AsyncReadExt;
    use tokio::sync::mpsc;
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn()?;
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    for stream in [
        child
            .stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Send + Unpin>),
        child
            .stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Send + Unpin>),
    ]
    .into_iter()
    .flatten()
    {
        let tx = tx.clone();
        tokio::spawn(async move {
            let (mut stream, mut buf, mut pending) = (stream, [0u8; 2048], String::new());
            while let Ok(n) = stream.read(&mut buf).await {
                if n == 0 {
                    break;
                }
                pending.push_str(&String::from_utf8_lossy(&buf[..n]));
                while let Some(i) = pending.find(['\r', '\n']) {
                    let line = strip_ansi(&pending[..i]).trim().to_string();
                    pending.drain(..=i);
                    if !line.is_empty() {
                        let _ = tx.send(line);
                    }
                }
            }
            let line = strip_ansi(&pending).trim().to_string();
            if !line.is_empty() {
                let _ = tx.send(line);
            }
        });
    }
    drop(tx);
    let run = async {
        while let Some(line) = rx.recv().await {
            on_line(line);
        }
        child.wait().await
    };
    let status = tokio::time::timeout(Duration::from_secs(600), run)
        .await
        .map_err(|_| anyhow::anyhow!("`{cmdline}` is taking too long; stopped waiting."))??;
    anyhow::ensure!(status.success(), "`{cmdline}` failed ({status})");
    Ok(())
}

/// What the person sends a running turn.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Steer {
    /// More to say; the agent reads it at its next step.
    Message(String),
    /// Don't wait for the running tool: stop it and read what's queued now.
    Interrupt,
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
    steering: Mutex<HashMap<String, mpsc::UnboundedSender<Steer>>>,
    /// The receiving end of a turn's steering channel, parked from the moment
    /// the turn is marked running until its runner picks it up, so messages
    /// sent while the agent is still starting wait in the queue.
    steering_rx: Mutex<HashMap<String, mpsc::UnboundedReceiver<Steer>>>,
    /// Messages that arrived after a turn stopped taking steering (its result
    /// is in, the process hasn't exited). They run as the next turn.
    followups: Mutex<HashMap<String, Vec<String>>>,
    approvals: Mutex<HashMap<String, (Approval, oneshot::Sender<Decision>)>>,
    /// OpenCode servers by chat id (see `opencode::server`).
    opencode: tokio::sync::Mutex<HashMap<String, Arc<opencode::Server>>>,
    /// Chats under this folder work on code nobody here wrote (the cloud
    /// runner's clones), so they get the review policy.
    review_root: RwLock<Option<PathBuf>>,
}

/// The checkout a dev build of tab runs from, so agents can be told not to
/// edit it under the running app. None in release builds.
fn dev_checkout() -> Option<String> {
    crate::sandbox::dev_checkout().map(|p| p.display().to_string())
}

/// Arguments for an agent CLI, collected before the sandbox wraps them.
#[derive(Default)]
pub(crate) struct Args(pub Vec<std::ffi::OsString>);

impl Args {
    pub fn arg(&mut self, a: impl AsRef<std::ffi::OsStr>) -> &mut Self {
        self.0.push(a.as_ref().to_owned());
        self
    }

    pub fn args<I, S>(&mut self, it: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        for a in it {
            self.arg(a);
        }
        self
    }
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
            steering_rx: Mutex::new(HashMap::new()),
            followups: Mutex::new(HashMap::new()),
            approvals: Mutex::new(HashMap::new()),
            opencode: tokio::sync::Mutex::new(HashMap::new()),
            review_root: RwLock::new(None),
        })
    }

    /// Treats every chat under `root` as work on someone else's code: the
    /// review policy, model keys only, no git host token.
    pub fn review_under(&self, root: &Path) {
        *self.review_root.write().unwrap() = Some(crate::sandbox::real(root));
    }

    pub async fn set_config(&self, cfg: AgentConfig) {
        let changed = *self.cfg.read().unwrap() != cfg;
        *self.cfg.write().unwrap() = cfg;
        if changed {
            for (_, old) in self.opencode.lock().await.drain() {
                old.shutdown();
            }
        }
    }

    /// Stops every OpenCode server; the next turn starts a fresh one with
    /// the current sandbox settings.
    pub async fn stop_opencode(&self) {
        for (_, s) in self.opencode.lock().await.drain() {
            s.shutdown();
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

    /// Each chat's own folder (system prompt, MCP config).
    pub fn sessions_dir(&self) -> PathBuf {
        self.dir.join("sessions")
    }

    /// Clears out attachments and chat folders unused for `keep`.
    pub fn sweep_files(
        &self,
        now: DateTime<Utc>,
        keep: std::time::Duration,
    ) -> anyhow::Result<crate::storage::Swept> {
        use crate::storage::{sweep_attachments, sweep_sessions};
        let mut swept = sweep_attachments(&self.store, &self.attachments_dir(), now, keep)?;
        swept.add(sweep_sessions(
            &self.store,
            &self.sessions_dir(),
            now,
            keep,
        )?);
        Ok(swept)
    }

    /// Every attachment with when it was last used, for the size limit.
    pub fn attachments(
        &self,
        now: DateTime<Utc>,
    ) -> anyhow::Result<Vec<crate::storage::Candidate<PathBuf>>> {
        crate::storage::attachments(&self.store, &self.attachments_dir(), now)
    }

    pub(crate) fn session_dir(&self, id: &str) -> anyhow::Result<PathBuf> {
        let d = self.sessions_dir().join(id);
        std::fs::create_dir_all(&d)?;
        Ok(d)
    }

    /// What a session's agent may touch. A chat about a pull request runs
    /// in tab's checkout of it, someone else's code, so it gets the review
    /// policy, as does one under [`AgentHub::review_under`] and a workflow's
    /// agent job (nobody is at the keyboard, and its prompt may carry text
    /// from an item: it pushes through tab's broker, not with the person's
    /// token); every other chat is the person's own.
    pub fn policy(&self, session: &Session) -> crate::sandbox::Policy {
        let sb = crate::sandbox::current();
        let cwd = crate::sandbox::real(Path::new(&session.cwd));
        let workspaces = crate::sandbox::real(&self.dir.join("workspaces"));
        let reviewed = self
            .review_root
            .read()
            .unwrap()
            .as_ref()
            .is_some_and(|r| cwd.starts_with(r));
        let job = self
            .store
            .session_key(&session.id)
            .ok()
            .flatten()
            .is_some_and(|k| k.starts_with("job:"));
        let mut p = if cwd.starts_with(&workspaces) || reviewed || job {
            sb.review_policy(&cwd, Path::new(&self.workdir()))
        } else {
            sb.chat_policy()
        };
        // Other chats' folders hold their tokens: each chat sees only its own
        // (made now, so the sandbox can show it).
        let _ = self.session_dir(&session.id);
        p.hide.push(crate::sandbox::real(&self.sessions_dir()));
        p.reveal
            .push(crate::sandbox::real(&self.sessions_dir().join(&session.id)));
        // gh and git's network commands go through tab, which needs its
        // server to be up.
        if self.mcp().is_some() {
            sb.brokered(p)
        } else {
            p
        }
    }

    /// Writes a chat's `gh` and `git` shims and points `cmd` at them, when
    /// the sandbox is on. Returns whether it did.
    pub(crate) fn add_shims(&self, session: &Session, cmd: &mut tokio::process::Command) -> bool {
        let sb = crate::sandbox::current();
        let (Some(mcp), true) = (self.mcp(), sb.config().enabled) else {
            return false;
        };
        let Ok(exe) = std::env::current_exe() else {
            return false;
        };
        let Ok(dir) = self.session_dir(&session.id) else {
            return false;
        };
        let url = mcp.url.trim_end_matches("/mcp").to_string() + "/cli";
        let chat_token = crate::mcp::chat_token(&mcp.token, &session.id);
        let ep = crate::sandbox::broker::Endpoint {
            url: &url,
            token: &chat_token,
            session: &session.id,
        };
        match crate::sandbox::broker::write_shims(&dir, &ep, &exe) {
            Ok(Some(shims)) => {
                shims.apply(cmd);
                true
            }
            Ok(None) => false,
            Err(e) => {
                tracing::warn!(error = %e, "writing CLI shims failed");
                false
            }
        }
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

    /// Whether this device runs the session (older sessions, from before
    /// sessions synced, name no device and are this one's).
    pub fn runs_here(&self, session: &Session) -> bool {
        match &session.device {
            None => true,
            Some(d) => self.store.device_id().is_ok_and(|me| me == *d),
        }
    }

    pub fn is_running(&self, session_id: &str) -> bool {
        self.running.lock().unwrap().contains_key(session_id)
    }

    /// How many agent turns are running right now. The dev runner waits
    /// for this to reach zero before it restarts the app.
    pub fn running_count(&self) -> usize {
        self.running.lock().unwrap().len()
    }

    /// Whether the running turn can take a message mid-flight.
    pub fn can_steer(&self, session_id: &str) -> bool {
        self.steering.lock().unwrap().contains_key(session_id)
    }

    /// Hands the person's message to the turn already running, so the agent
    /// picks it up at its next step instead of after it finishes. Returns
    /// false when no live turn takes messages (it already finished).
    pub fn steer(&self, session_id: &str, text: &str) -> anyhow::Result<bool> {
        self.steer_with(session_id, text, &[])
    }

    /// [`Self::steer`] with files the person attached (paths in
    /// [`Self::attachments_dir`]): the agent is told to open them, just as
    /// when they start a turn.
    pub fn steer_with(
        &self,
        session_id: &str,
        text: &str,
        attachments: &[String],
    ) -> anyhow::Result<bool> {
        let text = text.trim();
        anyhow::ensure!(
            !text.is_empty() || !attachments.is_empty(),
            "message is empty"
        );
        self.check_attachments(attachments)?;
        let prompt = with_attachments(text, attachments);
        let tx = self.steering.lock().unwrap().get(session_id).cloned();
        let sent = tx.is_some_and(|tx| tx.send(Steer::Message(prompt.clone())).is_ok());
        if !sent {
            // Held under the running lock so a turn that is finishing either
            // sees this message or has already finished (and we return false).
            let running = self.running.lock().unwrap();
            if !running.contains_key(session_id) {
                return Ok(false);
            }
            self.followups
                .lock()
                .unwrap()
                .entry(session_id.into())
                .or_default()
                .push(prompt);
        }
        let meta = if attachments.is_empty() {
            serde_json::json!({ "steer": true })
        } else {
            serde_json::json!({
                "steer": true,
                "attachments": attachments,
                "files": crate::live::file_refs(attachments),
            })
        };
        self.post(session_id, "user", "text", text, meta);
        Ok(true)
    }

    /// "Send now": cuts short the tool the running turn is waiting on, like
    /// Esc in Claude Code's terminal, so steering already sent is read right
    /// away instead of when the tool ends. False when no turn takes steering.
    pub fn interrupt(&self, session_id: &str) -> bool {
        let tx = self.steering.lock().unwrap().get(session_id).cloned();
        tx.is_some_and(|tx| tx.send(Steer::Interrupt).is_ok())
    }

    /// Attachments must be files in [`Self::attachments_dir`].
    fn check_attachments(&self, attachments: &[String]) -> anyhow::Result<()> {
        let dir = self.attachments_dir();
        for a in attachments {
            anyhow::ensure!(
                Path::new(a).starts_with(&dir) && Path::new(a).is_file(),
                "that attachment is missing"
            );
        }
        Ok(())
    }

    /// Opens the steering channel as soon as the turn is marked running.
    fn steering_prepare(&self, session_id: &str) {
        let (tx, rx) = mpsc::unbounded_channel();
        self.steering.lock().unwrap().insert(session_id.into(), tx);
        self.steering_rx
            .lock()
            .unwrap()
            .insert(session_id.into(), rx);
    }

    /// The turn runner's end of the channel opened by `steering_prepare`.
    pub(crate) fn steering_open(&self, session_id: &str) -> mpsc::UnboundedReceiver<Steer> {
        if let Some(rx) = self.steering_rx.lock().unwrap().remove(session_id) {
            return rx;
        }
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
        let me = self.store.device_id().ok();
        for s in sessions.into_iter().filter(|s| {
            s.status == SessionStatus::Working
                && s.device.as_ref().is_none_or(|d| me.as_ref() == Some(d))
        }) {
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
        anyhow::ensure!(
            self.runs_here(&session),
            "this chat runs on another device; steer it through the hub"
        );
        let text = text.trim();
        anyhow::ensure!(
            !text.is_empty() || !attachments.is_empty(),
            "message is empty"
        );
        self.check_attachments(attachments)?;
        if let Some(why) = crate::config::folder_problem(&session.cwd) {
            anyhow::bail!(
                "This chat starts in a folder tab can't use: {why} Recreate it, or set a work folder \
                 that exists in Settings → Agents and start a new chat."
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
            self.steering_prepare(session_id);
        }
        if visible {
            let meta = if attachments.is_empty() {
                serde_json::Value::Null
            } else {
                // `files` names them in the hub, so other devices can show them.
                serde_json::json!({
                    "attachments": attachments,
                    "files": crate::live::file_refs(attachments),
                })
            };
            self.post(session_id, "user", "text", text, meta);
            self.attach_pasted_links(session_id, text);
            // Name a new chat from its opening message now, rather than
            // after a first turn that can run for minutes.
            titler::spawn(self.clone(), session_id.into(), titler::Pass::Early);
        }
        self.update(session_id, Some(SessionStatus::Working), None);

        let hub = self.clone();
        let prompt = with_attachments(text, attachments);
        tokio::spawn(async move {
            let system = hub.system_prompt(&session);
            let result = match session.agent {
                AgentKind::ClaudeCode => {
                    claude::run_turn(&hub, &session, &prompt, &system, cancel.clone()).await
                }
                AgentKind::OpenCode => {
                    opencode::run_turn(&hub, &session, &prompt, &system, cancel.clone()).await
                }
                AgentKind::Codex => {
                    codex::run_turn(&hub, &session, &prompt, &system, cancel.clone()).await
                }
                AgentKind::Pi => {
                    pi::run_turn(&hub, &session, &prompt, &system, cancel.clone()).await
                }
            };
            let followups = {
                let mut running = hub.running.lock().unwrap();
                running.remove(&session.id);
                hub.steering_close(&session.id);
                hub.steering_rx.lock().unwrap().remove(&session.id);
                hub.followups
                    .lock()
                    .unwrap()
                    .remove(&session.id)
                    .unwrap_or_default()
            };
            // Anything still waiting on the person is moot now.
            hub.drop_approvals(&session.id);
            // However the turn ended, the thread now says more about the work.
            titler::spawn(hub.clone(), session.id.clone(), titler::Pass::TurnEnded);
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
                Ok(()) if !followups.is_empty() => {
                    // Already shown in the chat when they were sent.
                    if let Err(e) = hub.start_turn(&session.id, &followups.join("\n\n"), false, &[])
                    {
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
                Ok(()) => {
                    hub.mark_waiting_if_asked(&session.id);
                    suggest::spawn(hub.clone(), session.id.clone());
                }
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

    /// A turn that ends on the agent asking the person something ("Want me
    /// to open a PR?") is waiting on them, not idle.
    fn mark_waiting_if_asked(&self, session_id: &str) {
        let Ok(Some(s)) = self.store.session_get(session_id) else {
            return;
        };
        if s.status != SessionStatus::Idle {
            return;
        }
        let Ok(messages) = self.store.messages(session_id) else {
            return;
        };
        let asked = messages
            .iter()
            .rev()
            .find(|m| !matches!(m.kind.as_str(), "result" | "status" | "suggestion"))
            .is_some_and(|m| {
                m.role == "assistant" && m.kind == "text" && ends_with_question(&m.content)
            });
        if asked {
            self.update(session_id, Some(SessionStatus::NeedsInput), None);
        }
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

    /// Attaches the links in a creating call's result (a PR, an issue, a ticket) to the session's task
    /// as Context, so something the agent creates never depends on it remembering to.
    pub(crate) fn attach_created_links(&self, session_id: &str, output: &str) {
        let links = crate::links::found(output)
            .into_iter()
            .map(|(url, label)| (url, Some(label)))
            .collect();
        self.attach_links(session_id, links, "agent");
    }

    /// Attaches the links the person pastes into a message: a PR or issue, or
    /// anything that is itself in their queue. Other URLs are left to the agent.
    fn attach_pasted_links(&self, session_id: &str, text: &str) {
        let links = crate::links::urls(text)
            .into_iter()
            .map(|url| {
                let label = crate::links::label(&url);
                (url, label)
            })
            .collect();
        self.attach_links(session_id, links, "user");
    }

    /// Adds each `(url, label)` to the session's task Context. A URL whose
    /// queue item is known is attached as that item; one with no label is
    /// attached only when it is exactly a queue item's URL.
    fn attach_links(&self, session_id: &str, links: Vec<(String, Option<String>)>, by: &str) {
        let Some(item_id) = self
            .store
            .session_get(session_id)
            .ok()
            .flatten()
            .and_then(|s| s.item_id)
        else {
            return;
        };
        let mut changed = false;
        for (url, label) in links {
            let linked = self.store.item_by_url(&url).ok().flatten().filter(|i| {
                label.is_some()
                    || i.event.url.as_deref().map(|u| u.trim_end_matches('/'))
                        == Some(url.trim_end_matches('/'))
            });
            if linked.as_ref().is_some_and(|i| i.id == item_id) {
                continue;
            }
            let Some(title) = linked.as_ref().map(|i| i.event.title.clone()).or(label) else {
                continue;
            };
            let new = NewContext {
                section: ContextSection::Context,
                title,
                ref_item_id: linked.map(|i| i.id),
                url: Some(url),
                summary: None,
                added_by: by.into(),
            };
            match self.store.context_add(&item_id, &new, Utc::now()) {
                Ok(_) => changed = true,
                Err(e) => tracing::warn!(error = %e, "couldn't attach a link"),
            }
        }
        if changed {
            self.emit(EngineEvent::ContextChanged { item_id });
        }
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
             - Step list (required): whenever answering needs tools, your FIRST tool call is tab_steps with \
             your plan as 2-6 short steps (the first in_progress, the rest pending). It is a plan, not a \
             status line: list every step from now to the finished result for this turn (e.g. find it, \
             fix it, check it, open a PR), not only the step you're on, and never reveal steps one at a \
             time. Write each step as an outcome in the person's words, e.g. 'Find where Done is handled', \
             'Fix the badge layout', 'Open a PR': never a command, tool name, file path or flag. Call \
             tab_steps again each time a step finishes or the plan changes, and once at the end with every \
             step completed. The person sees this list at the end of the chat instead of your raw tool \
             calls, so never skip it. \
             Only a reply that needs no tools at all can go without it. If tab_steps isn't loaded, load it \
             with ToolSearch (select:mcp__tab__tab_steps).\n\
             - To show the person an image or a file (a screenshot you took, a PDF or report you wrote), \
             put its absolute path in backticks, like `/full/path/shot.png`: tab shows images inline and \
             other files as a card they can open. Don't just describe it; show it.\n\
             - If the work splits into follow-ups, create them with tab_create_task.\n\
             - Waiting on CI, a deploy or anything else slow: don't block on it with sleep or a --watch \
             command in the foreground. If you can, start the wait in the background (e.g. \
             run_in_background), say what you're waiting for and end your turn; tab shows the chat as \
             Monitoring and you pick back up when it finishes.\n\
             - tab_search matches by keyword and by meaning. Give it a window (within_days, or since/until) \
             and project: \"current\" when the person means recent or this project's work; it widens on its \
             own if the window is too narrow.\n\
             - When the person mentions something you don't recognise (a project, a codename, a person, a \
             repo), call tab_graph_search before asking them: it searches everything tab knows and shows \
             what's connected to it. tab_graph_expand follows one connection further.\n\
             - Projects: when this chat is in a project, call tab_project before starting work, and again \
             whenever you need to know where to work or how. It returns what the project is, its rules (follow \
             them), the repos its work happens in and where they're checked out, its notes and its recent work. \
             Work in the listed repos rather than guessing. Any project can be read by name with tab_project.\n\
             - Keep the project's context current for the next chat. When you work in a repo for the project \
             that isn't listed, add it with tab_project_repo (with its local path if you know it). If the \
             project has no description, or the person tells you what it is or how to work in it, write it with \
             tab_project_update (about, rules); each replaces the whole field, so keep what's still true. \
             When you learn a durable fact (how it deploys, who owns what, where things live, a decision \
             and why), save it with tab_project_note under a short topic, citing the items \
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
             - To show how something looks (a UI change, a mock, a chart), put an image in your reply: \
             tab's chat renders markdown images inline, including local files, as ![alt](C:\\absolute\\path.png), \
             and opens them in a viewer on click. Screenshot it (a headless browser works, or the running \
             app) and embed the PNG rather than describing it or publishing a separate page. Say whether it \
             is a real screenshot or a mock.\n\
             Keep replies short; the person reads them in a side panel.\n",
        );
        if let Some(root) = dev_checkout() {
            out.push_str(&format!(
                "\nThis is a dev build of tab, running from {root}. That checkout is read-only to you: each \
                 save would hot-reload or rebuild the app you are running inside, and a half-written edit can \
                 blank it or end this chat. To change tab's code, work in a separate worktree (git -C \"{root}\" \
                 worktree add ../tab-<topic> -b <branch> origin/main), and commit, push and open a PR from \
                 there.\n"
            ));
        }
        let sb = crate::sandbox::current();
        if sb.config().enabled {
            out.push_str(if sb.enforced() {
                "\nYou run in tab's sandbox. tab's own files, shell startup files and stored cloud and browser \
                 credentials are off limits, and in a pull request's checkout you can only change that checkout. \
                 A write that fails with \"Read-only file system\" or \"Operation not permitted\" on one of those \
                 paths is the sandbox: tell the person what you needed instead of working around it.\n\
                 A sandbox can't be started inside this one, so Chrome and Chromium fail with \"sandbox \
                 initialization failed\". Start them with --no-sandbox (Puppeteer: args: ['--no-sandbox']); \
                 tab's sandbox still applies to the browser.\n"
            } else {
                "\ntab's own files and stored credentials are off limits to your file tools. If an edit is refused \
                 for that reason, tell the person what you needed instead of working around it.\n"
            });
            if sb.config().enabled && self.mcp().is_some() {
                out.push_str(
                    "Use `gh` and `git` as usual: tab runs `gh` for you, and git reaches GitHub through tab, \
                     both signed in with the person's GitHub connection. There's no GitHub token in your \
                     environment, so don't look for one or change git's remote or credential settings. Force \
                     pushes, branch deletes and merges wait for the person to allow them; if one is declined, \
                     say what you wanted to do instead of retrying.\n",
                );
            }
        }
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
                "Project: {} (call tab_project for what it is, its rules, repos and recent work)\n",
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
        if let Ok(mut oc) = self.opencode.try_lock() {
            for (_, s) in oc.drain() {
                s.shutdown();
            }
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

    /// Updates the configured agent the way it was installed.
    pub async fn upgrade(
        &self,
        on_line: impl Fn(String) + Send + Sync + 'static,
    ) -> anyhow::Result<()> {
        let p = self
            .health_probe()
            .await
            .ok_or_else(|| anyhow::anyhow!("No agent is set up."))?;
        let path = p
            .path
            .filter(|_| p.found)
            .ok_or_else(|| anyhow::anyhow!("The agent wasn't found."))?;
        run_upgrade(p.kind, &path, on_line).await
    }

    async fn health_probe(&self) -> Option<AgentProbe> {
        let cfg = self.config();
        Some(probe(cfg.kind?, cfg.command.as_deref()).await)
    }

    /// Looks for every harness tab can drive, in `harnesses.json` order, all
    /// at once since each asks its CLI for a version. Each is looked up the
    /// way chats start it: the app's PATH, then its install folders.
    pub async fn detect(&self) -> Vec<AgentProbe> {
        let cfg = self.config();
        let probes = catalog::harnesses()
            .iter()
            .filter_map(|h| h.kind())
            .map(|kind| {
                let custom = (cfg.kind == Some(kind))
                    .then(|| cfg.command.clone())
                    .flatten();
                async move { probe(kind, custom.as_deref()).await }
            });
        futures_util::future::join_all(probes).await
    }
}

/// The message as the agent sees it: the text, then the attached files to
/// open.
fn with_attachments(text: &str, attachments: &[String]) -> String {
    if attachments.is_empty() {
        return text.to_string();
    }
    format!(
        "{text}\n\nThe person attached {} (open each with your Read tool to see it; to change one, write the new version under the same file name in your working folder and name its path in your reply, and tab shows it in its place):\n{}",
        if attachments.len() == 1 {
            "a file"
        } else {
            "files"
        },
        attachments
            .iter()
            .map(|a| format!("- {a}"))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

pub(crate) fn default_command(kind: AgentKind) -> &'static str {
    &catalog::harness(kind).command
}

/// `kind`'s executable: the configured command for the harness in use, else
/// PATH, else where its installer puts it.
pub(crate) fn agent_exe(cfg: &AgentConfig, kind: AgentKind) -> anyhow::Result<PathBuf> {
    let custom = cfg
        .command
        .as_deref()
        .filter(|c| !c.trim().is_empty() && cfg.kind == Some(kind));
    find(kind, custom)
}

/// `custom`, else `kind`'s command on PATH, else where its installer puts
/// it. A custom command that's just the default name (`claude`) is looked for
/// in the install folders too.
fn find(kind: AgentKind, custom: Option<&str>) -> anyhow::Result<PathBuf> {
    let custom = custom
        .map(str::trim)
        .filter(|c| *c != default_command(kind));
    resolve(custom.unwrap_or(default_command(kind))).or_else(|e| {
        custom
            .is_none()
            .then(|| catalog::installed_elsewhere(kind))
            .flatten()
            .ok_or(e)
    })
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
    // Just installed: the app's PATH may not have its folder yet.
    let found = find(kind, custom);
    let mut p = AgentProbe {
        kind,
        found: false,
        path: None,
        version: None,
        error: None,
        install: None,
        upgrade: None,
        latest: None,
        install_command: None,
    };
    let exe = match found {
        Ok(e) => e,
        Err(e) => {
            p.error = Some(e.to_string());
            p.install_command = catalog::harness(kind).install_command();
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

/// Whether a tool call only waits on something outside the chat: watching
/// CI or a deploy, a long sleep, a polling loop. The chat shows Monitoring
/// while one runs, since the agent isn't doing anything itself.
pub(crate) fn is_wait(tool: &str, input: &serde_json::Value) -> bool {
    if tool != "Bash" || input["run_in_background"].as_bool() == Some(true) {
        return false;
    }
    let Some(cmd) = input["command"].as_str() else {
        return false;
    };
    let words: Vec<&str> = cmd
        .split(|c: char| c.is_whitespace() || matches!(c, ';' | '&' | '|' | '(' | ')'))
        .filter(|w| !w.is_empty())
        .collect();
    let has = |w: &str| words.contains(&w);
    let runs = |seq: [&str; 3]| words.windows(3).any(|w| w == seq);
    let watch = runs(["gh", "run", "watch"]) || runs(["gh", "pr", "checks"]) && has("--watch");
    let rollout = runs(["kubectl", "rollout", "status"]);
    let polling = (has("until") || has("while")) && has("sleep");
    // Only a long sleep: a short one is usually a pause between real steps.
    let long_sleep = words.windows(2).any(|w| {
        w[0] == "sleep"
            && w[1]
                .trim_end_matches('s')
                .parse::<f64>()
                .is_ok_and(|n| n >= 30.0)
    });
    watch || rollout || polling || long_sleep
}

/// Whether a reply ends by asking the person something: its last line
/// (ignoring trailing markdown) ends with a question mark.
fn ends_with_question(text: &str) -> bool {
    text.lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty() && !l.starts_with("```"))
        .map(|l| l.trim_end_matches(['*', '_', '`', ')', '"', '\'', ' ']))
        .is_some_and(|l| l.ends_with('?'))
}

#[cfg(test)]
mod wait_tests {
    use super::is_wait;
    use serde_json::json;

    fn bash(cmd: &str) -> bool {
        is_wait("Bash", &json!({ "command": cmd }))
    }

    #[test]
    fn spots_commands_that_only_wait() {
        assert!(bash(
            "sleep 20; gh pr checks 89 --repo o/r --watch --interval 15 2>&1 | tail -4"
        ));
        assert!(bash("gh run watch 123 --exit-status"));
        assert!(bash("until curl -sf https://x; do sleep 5; done"));
        assert!(bash("sleep 120"));
        assert!(bash("kubectl rollout status deploy/web"));
    }

    #[test]
    fn leaves_real_work_alone() {
        assert!(!bash("gh pr checks 89"));
        assert!(!bash("gh run view 123 --log-failed"));
        assert!(!bash("sleep 2 && curl localhost:3000"));
        assert!(!bash("cargo test --workspace"));
        assert!(!bash("pnpm vitest --watch=false"));
        assert!(!is_wait(
            "Bash",
            &json!({ "command": "gh run watch 1", "run_in_background": true })
        ));
        assert!(!is_wait("Read", &json!({ "file_path": "sleep 100" })));
    }
}

#[cfg(test)]
mod question_tests {
    use super::ends_with_question;

    #[test]
    fn spots_a_closing_question() {
        assert!(ends_with_question(
            "Two ways.\n\nWant me to build option 1 and open a PR?"
        ));
        assert!(ends_with_question("Which one? **Option A or B?**\n"));
        assert!(!ends_with_question(
            "Is it broken? No, it's fixed.\n\nDone."
        ));
        assert!(!ends_with_question("Opened the PR."));
        assert!(!ends_with_question(""));
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

    /// Agent jobs and chats on a cloud runner's clones work on text and
    /// code nobody at the keyboard checked: the review policy, so no git
    /// host token or credential reaches them.
    #[test]
    fn jobs_and_cloud_clones_get_the_review_policy() {
        let store = Arc::new(Store::in_memory().unwrap());
        let (tx, _rx) = broadcast::channel(16);
        let hub = AgentHub::new(
            store.clone(),
            tx,
            AgentConfig::default(),
            std::env::temp_dir(),
        );
        let now = chrono::Utc::now();
        let cwd = std::env::temp_dir().join("tab-policy-repo");
        let cwd_s = cwd.to_string_lossy().to_string();
        let chat = store
            .session_create(None, AgentKind::ClaudeCode, "Chat", &cwd_s, now)
            .unwrap();
        assert_eq!(hub.policy(&chat).trust, crate::sandbox::Trust::Person);
        let job = store
            .session_create_keyed("job:1", AgentKind::ClaudeCode, "Job", &cwd_s, now)
            .unwrap();
        assert_eq!(hub.policy(&job).trust, crate::sandbox::Trust::Agent);
        hub.review_under(&std::env::temp_dir());
        assert_eq!(hub.policy(&chat).trust, crate::sandbox::Trust::Agent);
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
mod link_tests {
    use super::*;
    use crate::model::IncomingEvent;
    use crate::store::Upsert;

    fn item(s: &Store, id: &str, url: &str) -> String {
        let e = IncomingEvent {
            source: "t".into(),
            external_id: id.into(),
            kind: "t.k".into(),
            title: id.into(),
            url: Some(url.into()),
            occurred_at: Utc::now(),
            ..Default::default()
        };
        match s.upsert(&e, Utc::now()).unwrap() {
            Upsert::Inserted(i) => i.id,
            _ => unreachable!(),
        }
    }

    #[test]
    fn pasted_and_created_links_attach_and_refresh_the_rail() {
        let store = Arc::new(Store::in_memory().unwrap());
        let (tx, mut rx) = broadcast::channel(16);
        let hub = AgentHub::new(
            store.clone(),
            tx,
            AgentConfig::default(),
            std::env::temp_dir(),
        );
        let task = item(&store, "task", "https://tab.local/task");
        let thread = item(&store, "thread", "https://slack.com/archives/C1/p1");
        let s = store
            .session_create(Some(&task), AgentKind::OpenCode, "t", ".", Utc::now())
            .unwrap();

        hub.attach_pasted_links(
            &s.id,
            "fix https://github.com/o/r/issues/4 from https://slack.com/archives/C1/p1, \
             see https://docs.rs/x and https://tab.local/task",
        );
        hub.attach_created_links(&s.id, "https://github.com/o/r/pull/5\n");

        let ctx = store.item_context(&task).unwrap().unwrap().entries;
        let urls: Vec<_> = ctx.iter().filter_map(|c| c.url.as_deref()).collect();
        assert_eq!(
            urls.len(),
            3,
            "issue, queued thread and created PR; not the doc or the task itself: {urls:?}"
        );
        assert!(
            ctx.iter()
                .any(|c| c.ref_item.as_ref().is_some_and(|i| i.id == thread))
        );
        assert!(matches!(
            rx.try_recv(),
            Ok(EngineEvent::ContextChanged { .. })
        ));
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

    #[test]
    fn installer_colour_codes_are_dropped() {
        assert_eq!(strip_ansi("\u{1b}[32mDone\u{1b}[0m in 3s"), "Done in 3s");
    }
}
