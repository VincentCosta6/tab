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
pub use coordinator::COORDINATOR_KEY_PREFIX;
mod claude;
mod codex;
pub mod coordinator;
mod opencode;
mod per_turn;
mod pi;
mod small;
pub mod steward;
mod suggest;
mod titler;

pub use titler::DEFAULT_RULES as TITLER_DEFAULT_RULES;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::NewContext;
use crate::config::AgentConfig;
use crate::engine::EngineEvent;
use crate::model::{AgentKind, Approval, ContextSection, ProjectNote, Session, SessionStatus};
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
/// One MCP server as the agent saw it at the start of a turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentMcpServer {
    pub name: String,
    #[serde(default)]
    pub status: Option<String>,
}

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

/// The newest version of an agent CLI that its update command would
/// install. A winget or Homebrew install updates from that package manager,
/// which can be a few releases behind npm; comparing it with npm would offer
/// an update that `winget upgrade` can't find. Everything else (npm, the
/// agent's own updater) follows npm. Cached for six hours per install
/// method; `None` when offline.
pub async fn latest_version(
    kind: AgentKind,
    install: Option<&str>,
    upgrade: Option<&str>,
) -> Option<String> {
    use std::time::Instant;
    let source: &'static str = match install {
        Some("winget") => "winget",
        Some("brew") => "brew",
        _ => "npm",
    };
    let at_key = (kind, source);
    if let Some((at, v)) = LATEST.lock().unwrap().get(&at_key)
        && at.elapsed() < Duration::from_secs(6 * 3600)
    {
        return Some(v.clone());
    }
    let version = match source {
        "winget" => winget_latest(upgrade?).await,
        "brew" => brew_latest(upgrade?).await,
        _ => npm_latest(kind).await,
    }?;
    LATEST
        .lock()
        .unwrap()
        .insert(at_key, (Instant::now(), version.clone()));
    Some(version)
}

type LatestCache = HashMap<(AgentKind, &'static str), (std::time::Instant, String)>;
static LATEST: std::sync::LazyLock<Mutex<LatestCache>> = std::sync::LazyLock::new(Default::default);

/// Forgets the newest versions looked up, so the next check asks again
/// (after an update, say).
pub fn forget_latest(kind: AgentKind) {
    LATEST.lock().unwrap().retain(|(k, _), _| *k != kind);
}

async fn npm_latest(kind: AgentKind) -> Option<String> {
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
    Some(v["version"].as_str()?.to_string())
}

/// Runs a package manager's query and returns what it printed, if it worked.
async fn query_output(program: &str, args: &[&str]) -> Option<String> {
    let exe = which::which(program).ok()?;
    let out = tokio::time::timeout(Duration::from_secs(20), command(&exe).args(args).output())
        .await
        .ok()?
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// What `winget upgrade <id>` would install: the package's newest version
/// in winget's sources.
async fn winget_latest(upgrade: &str) -> Option<String> {
    let id = upgrade.split_whitespace().last()?;
    let out = query_output(
        "winget",
        &[
            "show",
            "--id",
            id,
            "--exact",
            "--accept-source-agreements",
            "--disable-interactivity",
        ],
    )
    .await?;
    winget_show_version(&out)
}

/// The `Version:` line of `winget show`.
fn winget_show_version(out: &str) -> Option<String> {
    out.lines()
        .find_map(|l| l.trim().strip_prefix("Version:"))
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// What `brew upgrade [--cask] <name>` would install.
async fn brew_latest(upgrade: &str) -> Option<String> {
    let cask = upgrade.contains("--cask");
    let name = upgrade.split_whitespace().last()?;
    let mut args = vec!["info", "--json=v2"];
    if cask {
        args.push("--cask");
    }
    args.push(name);
    let v: serde_json::Value = serde_json::from_str(&query_output("brew", &args).await?).ok()?;
    let version = if cask {
        v["casks"][0]["version"].as_str()
    } else {
        v["formulae"][0]["versions"]["stable"].as_str()
    }?;
    // Casks can carry a build after a comma ("2.1.3,abc").
    Some(version.split(',').next()?.to_string())
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
    let winget = program == "winget";
    if winget {
        cmd.args([
            "--accept-source-agreements",
            "--accept-package-agreements",
            "--disable-interactivity",
        ]);
    }
    let r = run_streaming(cmd, cmdline, on_line).await;
    // Whatever happened, the next check asks again rather than offering
    // the same update from the cache.
    forget_latest(kind);
    match r {
        // winget has nothing newer: it already has the newest winget offers.
        Err(e) if winget && format!("{e}").contains(WINGET_NO_UPDATE) => Ok(()),
        r => r,
    }
}

/// `APPINSTALLER_CLI_ERROR_UPDATE_NOT_APPLICABLE`: `winget upgrade` found
/// nothing newer. Rust prints the exit status in hex on Windows.
const WINGET_NO_UPDATE: &str = "0x8a15002b";

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
    /// From tab, not the person: read like a message, but not one the
    /// agent owes a reply to (see `claude::Unanswered`).
    Reminder(String),
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
    /// Puts a file in the hub and returns its hash, once the hub is up, so
    /// files a reply names reach other devices ([`crate::named_files`]).
    file_sink: std::sync::OnceLock<FileSink>,
    /// Chats under this folder work on code nobody here wrote (the cloud
    /// runner's clones), so they get the review policy.
    review_root: RwLock<Option<PathBuf>>,
    /// Remote MCP servers the person connected (Linear, Notion, …).
    remote: Arc<crate::remote_mcp::RemoteMcp>,
}

/// See [`AgentHub::set_file_sink`].
pub type FileSink = Arc<dyn Fn(&[u8]) -> anyhow::Result<String> + Send + Sync>;

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
        let remote = crate::remote_mcp::RemoteMcp::new(dir.join("mcp-sign-ins"));
        Arc::new(Self {
            store,
            events,
            cfg: RwLock::new(cfg),
            mcp: RwLock::new(None),
            remote,
            dir,
            running: Mutex::new(HashMap::new()),
            steering: Mutex::new(HashMap::new()),
            steering_rx: Mutex::new(HashMap::new()),
            followups: Mutex::new(HashMap::new()),
            approvals: Mutex::new(HashMap::new()),
            opencode: tokio::sync::Mutex::new(HashMap::new()),
            file_sink: std::sync::OnceLock::new(),
            review_root: RwLock::new(None),
        })
    }

    /// Where files an agent's reply names go, so other devices can show them.
    pub fn set_file_sink(&self, sink: FileSink) {
        let _ = self.file_sink.set(sink);
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

    /// The config a chat's turn runs with: tab's, on the chat's own model.
    pub fn config_for(&self, session: &Session) -> AgentConfig {
        let mut cfg = self.config();
        if let Some(m) = &session.model {
            cfg.set_model(session.agent, Some(m.clone()));
        }
        cfg
    }

    /// Switches one chat's model (`None` for the harness's default), from
    /// its next message. Other chats keep theirs.
    pub fn set_session_model(
        &self,
        session_id: &str,
        model: Option<&str>,
    ) -> anyhow::Result<Session> {
        let session = self
            .store
            .session_set_model(session_id, model.unwrap_or(""))?
            .ok_or_else(|| anyhow::anyhow!("no such session"))?;
        self.emit(EngineEvent::SessionChanged {
            session: session.clone(),
        });
        Ok(session)
    }

    pub fn set_mcp(&self, endpoint: McpEndpoint) {
        *self.mcp.write().unwrap() = Some(endpoint);
    }

    pub(crate) fn mcp(&self) -> Option<McpEndpoint> {
        self.mcp.read().unwrap().clone()
    }

    /// Whether chats' `gh` and `git` go through tab's server. Not on a cloud
    /// runner: it gives agents no git host token at all, and pushes jobs'
    /// branches itself.
    fn brokers(&self) -> bool {
        self.mcp().is_some() && !self.remote.in_cloud()
    }

    /// The remote MCP servers the person connected.
    pub fn remote_mcp(&self) -> &Arc<crate::remote_mcp::RemoteMcp> {
        &self.remote
    }

    /// What a job's chat needed in tab cloud and didn't have.
    pub fn take_gaps(&self, session: &str) -> Vec<tab_domain::runner::CloudGap> {
        self.remote.take_gaps(session)
    }

    /// Remote MCP servers a chat gets, as `(name, url)` on tab's MCP
    /// server, which signs each request in. On a computer, only chats the
    /// person drives: an agent reading someone else's code or mail could be
    /// talked into acting as them. On a cloud runner, jobs get what the
    /// person allowed in tab cloud (writes only when they said so), and a
    /// stand-in for each connection that isn't there, so a job can say it
    /// needed one.
    pub(crate) fn remote_servers(&self, trust: crate::sandbox::Trust) -> Vec<(String, String)> {
        let Some(mcp) = self.mcp() else {
            return Vec::new();
        };
        let cloud = self.remote.in_cloud();
        if trust != crate::sandbox::Trust::Person && !cloud {
            return Vec::new();
        }
        let mut ids = self.remote.usable();
        if cloud {
            ids.extend(self.remote.stand_in_ids());
        }
        ids.into_iter()
            .map(|id| {
                let url = format!("{}/{id}", mcp.url.trim_end_matches('/'));
                (id, url)
            })
            .collect()
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

    /// The MCP servers a chat's agent reported when its last turn started,
    /// with their status (`connected`, `failed`, `needs-auth`, `pending`),
    /// for the composer's `/mcp` card.
    pub fn agent_mcp(&self, session: &str) -> Vec<AgentMcpServer> {
        std::fs::read(self.agent_mcp_path(session))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub(crate) fn save_agent_mcp(&self, session: &str, servers: &[AgentMcpServer]) {
        if self.agent_mcp(session) == servers {
            return;
        }
        let path = self.agent_mcp_path(session);
        if let (Some(dir), Ok(b)) = (path.parent(), serde_json::to_vec(servers)) {
            let _ = std::fs::create_dir_all(dir);
            let _ = std::fs::write(path, b);
        }
    }

    fn agent_mcp_path(&self, session: &str) -> PathBuf {
        // Session ids are uuids; keep anything else out of the path.
        let name: String = session
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
            .collect();
        self.dir.join("agent_mcp").join(format!("{name}.json"))
    }

    /// claude.ai connectors (`claude.ai Linear`) the last chat that could
    /// see them had.
    pub fn claude_ai_connectors(&self) -> Vec<String> {
        std::fs::read(self.dir.join("claude_ai_connectors.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub(crate) fn save_claude_ai_connectors(&self, names: &[String]) {
        if self.claude_ai_connectors() == names {
            return;
        }
        if let Ok(b) = serde_json::to_vec(names) {
            let _ = std::fs::write(self.dir.join("claude_ai_connectors.json"), b);
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
        // A confined chat of the person's own may use its project's repos
        // and their worktrees, wherever they're checked out.
        if !p.confine.is_empty() && p.trust == crate::sandbox::Trust::Person {
            p.open.extend(self.project_checkouts(session));
        }
        // Other chats' folders hold their tokens: each chat sees only its own
        // (made now, so the sandbox can show it).
        let _ = self.session_dir(&session.id);
        p.hide.push(crate::sandbox::real(&self.sessions_dir()));
        p.reveal
            .push(crate::sandbox::real(&self.sessions_dir().join(&session.id)));
        // gh and git's network commands go through tab, which needs its
        // server to be up.
        if self.brokers() { sb.brokered(p) } else { p }
    }

    /// Where the chat's project's repos are checked out, with each one's
    /// git worktrees. An agent can name a repo's path itself (the
    /// `tab_project_repo` tool, or a record synced from another device) and
    /// write a checkout's worktree list, so anything wider than a code
    /// folder is left out ([`crate::sandbox::Sandbox::too_wide`]).
    fn project_checkouts(&self, session: &Session) -> Vec<PathBuf> {
        let sb = crate::sandbox::current();
        let project = session
            .item_id
            .as_deref()
            .and_then(|id| self.store.get(id).ok().flatten())
            .and_then(|i| i.project_id);
        let Some(project) = project else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for repo in self.store.project_repos(&project).unwrap_or_default() {
            let Some(path) = repo.path.filter(|p| !p.trim().is_empty()) else {
                continue;
            };
            let root = crate::sandbox::real(Path::new(&path));
            if sb.too_wide(&root) {
                tracing::warn!(path = %root.display(), "a project repo's path is too wide to open to agents");
                continue;
            }
            out.extend(
                crate::sandbox::worktrees(&root)
                    .into_iter()
                    .filter(|w| !sb.too_wide(w)),
            );
            out.push(root);
        }
        out
    }

    /// Writes a chat's `gh` and `git` shims and points `cmd` at them, when
    /// the sandbox is on. Returns whether it did.
    pub(crate) fn add_shims(&self, session: &Session, cmd: &mut tokio::process::Command) -> bool {
        let sb = crate::sandbox::current();
        let (Some(mcp), true) = (self.mcp(), sb.config().enabled && self.brokers()) else {
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
        mut meta: serde_json::Value,
    ) {
        if role == "assistant" && kind == "text" {
            self.share_named_files(session_id, content, &mut meta);
        }
        match self
            .store
            .message_add(session_id, role, kind, content, &meta, Utc::now())
        {
            Ok(message) => self.emit(EngineEvent::SessionMessage { message }),
            Err(e) => tracing::error!(error = %e, "storing session message failed"),
        }
    }

    /// Which files a chat's replies may share to the hub. tab reads them
    /// itself, outside the sandbox, so a reply naming a file mustn't reach
    /// anything the agent couldn't: only files (links resolved) under the
    /// chat's own folder, its project's repos, Settings' code folders or
    /// the attachments folder, and nothing the chat's sandbox hides
    /// (credentials, tab's own files, other chats' folders), wherever the
    /// sandbox is enforced or not.
    fn shareable(&self, session: &Session) -> Box<dyn Fn(&Path) -> bool> {
        let sb = crate::sandbox::current();
        let mut policy = self.policy(session);
        let cwd = crate::sandbox::real(Path::new(&session.cwd));
        let mut roots = Vec::new();
        if !sb.too_wide(&cwd) {
            roots.push(cwd);
        }
        roots.extend(self.project_checkouts(session));
        if policy.trust == crate::sandbox::Trust::Person {
            roots.extend(sb.code_folders());
        }
        roots.push(crate::sandbox::real(&self.attachments_dir()));
        // The roots are what the agent may use (its own folder is opened
        // when it starts); what's hidden inside them still wins.
        policy.open.extend(roots.iter().cloned());
        let secrets = sb.tab_secrets();
        let sessions = crate::sandbox::real(&self.sessions_dir());
        Box::new(move |p: &Path| {
            roots.iter().any(|r| p.starts_with(r))
                && !sb.too_wide(p)
                && !p.starts_with(&sessions)
                && !secrets.iter().any(|s| p.starts_with(s))
                && policy.allows(p, crate::sandbox::Access::Read)
        })
    }

    /// Puts the local files a reply names in the hub and records them in
    /// its `files`, so a device that isn't this one can show them.
    fn share_named_files(&self, session_id: &str, text: &str, meta: &mut serde_json::Value) {
        let Some(sink) = self.file_sink.get() else {
            return;
        };
        let Some(session) = self.store.session_get(session_id).ok().flatten() else {
            return;
        };
        let cwd = Some(session.cwd.as_str());
        if crate::named_files::paths_in_text(text, cwd).is_empty() {
            return;
        }
        let shareable = self.shareable(&session);
        let files = crate::named_files::share(text, cwd, |p| shareable(p), |b| sink(b));
        if files.is_empty() {
            return;
        }
        if !meta.is_object() {
            *meta = serde_json::json!({});
        }
        meta["files"] = serde_json::to_value(files).unwrap_or_default();
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

    /// Tells a running turn something from tab itself: read like a steering
    /// message, but not shown in the chat as the person's.
    pub(crate) fn remind(&self, session_id: &str, text: String) {
        if let Some(tx) = self.steering.lock().unwrap().get(session_id) {
            let _ = tx.send(Steer::Reminder(text));
        }
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
        // A chat keeps the model it starts on, whatever other chats pick later.
        let session = match session.model {
            Some(_) => session,
            None => {
                let m = self
                    .config()
                    .model_for(session.agent)
                    .unwrap_or("")
                    .to_string();
                self.set_session_model(session_id, Some(&m))?
            }
        };
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

    /// Answers approval `id` only if it's one of `session_id`'s, for
    /// answers that come from elsewhere (another device steering one
    /// chat mustn't answer another chat's).
    pub fn resolve_approval_in(&self, session_id: &str, id: &str, decision: Decision) -> bool {
        let mut approvals = self.approvals.lock().unwrap();
        if !approvals
            .get(id)
            .is_some_and(|(a, _)| a.session_id == session_id)
        {
            return false;
        }
        match approvals.remove(id) {
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
             - A plan, design or architecture write-up longer than a few lines goes in a markdown file \
             (in your working folder), named by its path in backticks: tab shows it as a card the person \
             opens to read, and it travels with the chat. Keep the reply itself short.\n\
             - If the work splits into follow-ups, create them with tab_create_task; each is linked with \
             this task both ways, so don't attach it again. Give each a body the next agent can start from. \
             When the person asks for agents on them, pass start_agent with the first message.\n\
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
             Project notes are rare: every future chat reads them. Save one with tab_project_note only \
             for a fact the next chat on an unrelated task would need and couldn't get from the repo (where \
             it's checked out, a non-obvious way to build or deploy, an environment limit, who owns what, a \
             decision and why). Never write up what you built or how a feature works, file maps, PR \
             changelogs, task status, one-off events, credentials or personal details: the code and git \
             hold those. The project's steward reviews each note and may rewrite or turn it down; accept \
             its answer.\n\
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
             - The person can send messages while you work; they reach you between tool calls. Answer \
             each one in visible reply text (not only in your thinking, which tab never shows them) \
             before your next tool call, even if it's one line, then carry on.\n\
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
            if sb.config().enabled && self.brokers() {
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
            out.push_str(&notes_block(
                &self.store.project_notes(&p.id).unwrap_or_default(),
            ));
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
            p.latest = latest_version(kind, p.install.as_deref(), p.upgrade.as_deref()).await;
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

/// Notes shown in full at the start of a chat; the rest are named only.
const NOTES_IN_PROMPT: usize = 12;

/// The project's notes as a chat starts with them: the person's first, then
/// the newest agent notes, each in full up to [`NOTES_IN_PROMPT`]. Any more
/// are listed by topic so the agent knows to read them with tab_project.
fn notes_block(notes: &[ProjectNote]) -> String {
    if notes.is_empty() {
        return String::new();
    }
    let mut ordered: Vec<&ProjectNote> = notes.iter().filter(|n| n.added_by != "agent").collect();
    ordered.extend(notes.iter().filter(|n| n.added_by == "agent"));
    let mut out = String::from("Project notes:\n");
    for n in ordered.iter().take(NOTES_IN_PROMPT) {
        let text: String = n.text.chars().take(steward::MAX_NOTE_CHARS + 1).collect();
        out.push_str(&format!("- {}: {}\n", n.topic, text.trim()));
    }
    let rest: Vec<&str> = ordered
        .iter()
        .skip(NOTES_IN_PROMPT)
        .map(|n| n.topic.as_str())
        .collect();
    if !rest.is_empty() {
        out.push_str(&format!(
            "More notes, in tab_project: {}\n",
            rest.join(", ")
        ));
    }
    out
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
mod notes_tests {
    use super::*;
    use chrono::Utc;

    fn note(topic: &str, by: &str) -> ProjectNote {
        ProjectNote {
            id: topic.into(),
            project_id: "p".into(),
            topic: topic.into(),
            text: format!("fact about {topic}"),
            added_by: by.into(),
            session_id: None,
            sources: vec![],
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn person_notes_lead_and_extras_are_named_only() {
        assert_eq!(notes_block(&[]), "");
        let mut notes: Vec<ProjectNote> = (0..NOTES_IN_PROMPT + 2)
            .map(|i| note(&format!("a{i}"), "agent"))
            .collect();
        notes.push(note("owners", "user"));
        let s = notes_block(&notes);
        let lines: Vec<&str> = s.lines().collect();
        assert_eq!(
            lines[1], "- owners: fact about owners",
            "the person's note comes first"
        );
        assert_eq!(lines.len(), 1 + NOTES_IN_PROMPT + 1);
        assert!(
            lines
                .last()
                .unwrap()
                .starts_with("More notes, in tab_project: a11, a12, a13")
        );
        assert!(!s.contains("2026"), "no dates or authors");
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

    /// A chat keeps its own model: picking one in another chat, or changing
    /// tab's default, leaves it alone. Older chats follow the default.
    #[tokio::test]
    async fn chats_keep_their_own_model() {
        let store = Arc::new(Store::in_memory().unwrap());
        let (tx, _rx) = broadcast::channel(16);
        let mut cfg = AgentConfig {
            kind: Some(AgentKind::ClaudeCode),
            ..Default::default()
        };
        cfg.set_model(AgentKind::ClaudeCode, Some("opus".into()));
        let hub = AgentHub::new(store.clone(), tx, cfg.clone(), std::env::temp_dir());
        let a = hub.session_for_key("a", "A", "/a", "").unwrap();
        let b = hub.session_for_key("b", "B", "/b", "").unwrap();
        assert_eq!(
            hub.config_for(&a).model_for(AgentKind::ClaudeCode),
            Some("opus")
        );

        let a = hub.set_session_model(&a.id, Some("sonnet")).unwrap();
        let b = hub.set_session_model(&b.id, None).unwrap();
        cfg.set_model(AgentKind::ClaudeCode, Some("haiku".into()));
        hub.set_config(cfg).await;
        assert_eq!(
            hub.config_for(&a).model_for(AgentKind::ClaudeCode),
            Some("sonnet")
        );
        assert_eq!(hub.config_for(&b).model_for(AgentKind::ClaudeCode), None);
        let older = store
            .session_create(None, AgentKind::ClaudeCode, "t", ".", Utc::now())
            .unwrap();
        assert_eq!(older.model, None);
        assert_eq!(
            hub.config_for(&older).model_for(AgentKind::ClaudeCode),
            Some("haiku")
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
    fn winget_latest_reads_the_version_winget_offers() {
        let out = "Found Claude Code [Anthropic.ClaudeCode]
Version: 2.1.317
Publisher: Anthropic PBC
";
        assert_eq!(winget_show_version(out).as_deref(), Some("2.1.317"));
        assert_eq!(
            winget_show_version("No package found matching input criteria."),
            None
        );
    }

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
