//! Sandboxing for the processes tab starts on the person's behalf: agent
//! sessions, language servers on pull request checkouts, and workflow
//! command steps.
//!
//! Every such process goes through [`Sandbox::command`] with a [`Policy`]
//! that says where it may write, what it may not read, and where it may
//! connect. How that is enforced depends on the OS:
//!
//! | OS      | Files                         | Network                                  |
//! |---------|-------------------------------|------------------------------------------|
//! | macOS   | Seatbelt (`sandbox-exec`)     | Seatbelt: none, or only tab's proxy      |
//! | Linux   | bubblewrap (`bwrap`)          | a network namespace: none, or only tab's proxy |
//! | Windows | not enforced                  | not enforced                             |
//! | any, with an image | a Docker or Podman container | none, or the container's network |
//!
//! Whatever the OS, every process also gets:
//! * a scrubbed environment ([`env`]): tokens and cloud credentials in tab's
//!   own environment don't reach agents or commands;
//! * a process tree ([`tree`]): a job object on Windows, a process group on
//!   Unix, so stopping a chat or a step ends everything it started.
//!
//! Policies come in two shapes, both built here so every caller agrees:
//! * [`Sandbox::chat_policy`] for chats the person drives. Writes are allowed
//!   everywhere except protected places (tab's own checkout, tab's data and
//!   config, shell startup files, `~/.ssh`), and credential stores are
//!   hidden. Agents keep working the way they always have.
//! * [`Sandbox::review_policy`] / [`Sandbox::isolated_policy`] for code
//!   nobody here wrote: review chats, language servers and workflow
//!   commands. Writes are limited to the checkout or the run's folder, and
//!   git and SSH credentials are hidden too.

pub mod bridge;
pub mod broker;
mod bwrap;
pub mod container;
pub mod env;
pub mod proxy;
mod seatbelt;
pub mod tree;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, RwLock};

use serde::{Deserialize, Serialize};

pub use tree::Guarded;

/// Where a sandboxed process may connect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "domains", rename_all = "snake_case")]
pub enum Network {
    Any,
    None,
    /// Only these hosts, through tab's proxy. `*.example.com` matches
    /// subdomains; `*` alone means any.
    Domains(Vec<String>),
}

impl Network {
    /// From a step's list: empty is none, `*` is any.
    pub fn from_list(list: &[String]) -> Self {
        let list: Vec<String> = list
            .iter()
            .map(|d| d.trim().to_lowercase())
            .filter(|d| !d.is_empty())
            .collect();
        if list.is_empty() {
            Network::None
        } else if list.iter().any(|d| d == "*") {
            Network::Any
        } else {
            Network::Domains(list)
        }
    }
}

/// Whose process this is, which decides what's kept of the environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trust {
    /// A chat the person is driving: keeps the agent's own model keys and
    /// the GitHub token, so pushing and opening PRs still work.
    Person,
    /// An agent working on someone else's code: model keys only.
    Agent,
    /// A language server or workflow command: nothing secret.
    Untrusted,
}

/// What a sandboxed process may do.
#[derive(Debug, Clone, PartialEq)]
pub struct Policy {
    /// Never writable. `/` makes everything read-only except `writable`.
    pub protect: Vec<PathBuf>,
    /// Writable even inside a protected folder (the checkout's `.git`, the
    /// run folder, caches).
    pub writable: Vec<PathBuf>,
    /// Not readable at all.
    pub hide: Vec<PathBuf>,
    /// Readable even inside a hidden folder.
    pub reveal: Vec<PathBuf>,
    /// Readable and writable even inside a hidden folder: the agent's own
    /// transcript among every other chat's.
    pub reveal_rw: Vec<PathBuf>,
    /// Folders whose contents are off limits except for `open` and
    /// `readable` (the home folder, other people's, external drives).
    /// Their names still show to `stat`, so paths resolve; what's in
    /// them can't be read, listed or changed.
    pub confine: Vec<PathBuf>,
    /// Usable inside `confine`: the chat's folder, code folders, caches.
    /// `protect` and `hide` still apply inside them.
    pub open: Vec<PathBuf>,
    /// Readable but not writable inside `confine`: toolchains, shell and
    /// git setup.
    pub readable: Vec<PathBuf>,
    /// Programs that can't be started: the real `gh` when tab brokers it.
    pub block_exec: Vec<PathBuf>,
    /// Variables removed from the environment on top of what `trust` removes.
    pub drop_env: Vec<String>,
    /// tab runs this chat's GitHub CLIs (see [`Sandbox::brokered`]).
    pub brokered: bool,
    pub network: Network,
    pub trust: Trust,
}

/// What the person chose in Settings → Sandbox.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SandboxConfig {
    /// On by default. Off still scrubs the environment and keeps process
    /// trees, which never get in the way.
    #[serde(default = "yes")]
    pub enabled: bool,
    /// More folders agents may read but not change.
    #[serde(default)]
    pub protected: Vec<String>,
    /// More files or folders agents can't read at all.
    #[serde(default)]
    pub hidden: Vec<String>,
    /// Hosts language servers may reach to fetch dependencies.
    #[serde(default = "default_lsp_domains")]
    pub lsp_domains: Vec<String>,
    /// `docker` or `podman`; empty picks whichever is installed.
    #[serde(default)]
    pub container_runtime: String,
    /// Only let agents into the folders they need: the chat's own, code
    /// folders, toolchains and caches. Everything else in the home folder,
    /// other people's, external drives and tab's own folders can't be read
    /// or listed. On by default, on macOS and Linux.
    #[serde(default = "yes")]
    pub confine: bool,
    /// Folders agents may use when `confine` is on, on top of the chat's
    /// own folder and its project's repos.
    #[serde(default = "default_folders")]
    pub folders: Vec<String>,
    /// Whether chats may use the person's SSH keys. On by default, since
    /// people push to other hosts over SSH; off keeps `~/.ssh` and the SSH
    /// agent out of chats, so GitHub is only reachable through tab.
    #[serde(default = "yes")]
    pub ssh_in_chats: bool,
}

fn yes() -> bool {
    true
}

/// Where people usually keep code. Missing ones are harmless.
pub fn default_folders() -> Vec<String> {
    [
        "~/tmp",
        "~/src",
        "~/code",
        "~/dev",
        "~/Developer",
        "~/projects",
        "~/repos",
        "~/git",
        "~/work",
        "~/workspace",
    ]
    .map(String::from)
    .to_vec()
}

pub fn default_lsp_domains() -> Vec<String> {
    [
        "crates.io",
        "*.crates.io",
        "proxy.golang.org",
        "sum.golang.org",
        "pypi.org",
        "files.pythonhosted.org",
        "registry.npmjs.org",
        "github.com",
        "*.githubusercontent.com",
    ]
    .map(String::from)
    .to_vec()
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            protected: Vec::new(),
            hidden: Vec::new(),
            lsp_domains: default_lsp_domains(),
            container_runtime: String::new(),
            confine: true,
            folders: default_folders(),
            ssh_in_chats: true,
        }
    }
}

/// How this computer enforces policies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Backend {
    Seatbelt,
    Bubblewrap,
    /// Nothing to enforce with: Windows, or Linux without a working `bwrap`.
    Unavailable,
    /// tab itself runs inside a sandbox already (its tests run from a
    /// sandboxed chat, say). macOS can't start a sandbox inside another,
    /// so processes start directly, and the outer sandbox's limits still
    /// apply to them.
    Inherited,
}

impl Backend {
    /// Whether tab applies its own policies with it.
    pub fn enforces(self) -> bool {
        matches!(self, Backend::Seatbelt | Backend::Bubblewrap)
    }
}

/// For Settings and the chat header.
#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub enabled: bool,
    pub backend: Backend,
    /// Whether file and network limits are actually enforced here.
    pub enforced: bool,
    /// Plain words for the person.
    pub summary: String,
    /// What would make it enforced, when it isn't.
    pub fix: Option<String>,
    /// `docker` or `podman`, when one answers.
    pub container: Option<String>,
}

/// A command ready to spawn, and what applies to it.
pub struct Prepared {
    pub cmd: tokio::process::Command,
    /// Whether the policy's file and network limits are enforced.
    pub enforced: bool,
    proxy: Option<proxy::Handle>,
}

impl Prepared {
    /// Starts it in its own process tree. Dropping the result ends the
    /// whole tree.
    pub fn spawn(mut self) -> std::io::Result<Guarded> {
        tree::prepare(&mut self.cmd);
        let child = self.cmd.spawn().map_err(|e| explain_spawn(&self.cmd, e))?;
        Ok(Guarded::new(child, self.proxy.take()))
    }
}

/// "No such file or directory" from a spawn can mean the program or the
/// folder it starts in; say which.
fn explain_spawn(cmd: &tokio::process::Command, e: std::io::Error) -> std::io::Error {
    if e.kind() != std::io::ErrorKind::NotFound {
        return e;
    }
    let std = cmd.as_std();
    if let Some(dir) = std.get_current_dir().filter(|d| !d.is_dir()) {
        return std::io::Error::new(
            e.kind(),
            format!(
                "the folder it starts in, {}, doesn't exist ({e})",
                dir.display()
            ),
        );
    }
    let program = Path::new(std.get_program());
    if program.components().count() > 1 && !program.exists() {
        return std::io::Error::new(
            e.kind(),
            format!("{} doesn't exist ({e})", program.display()),
        );
    }
    e
}

pub struct Sandbox {
    cfg: RwLock<SandboxConfig>,
    backend: Backend,
    /// What [`detect`] found, when it isn't a sandbox tab can use.
    detected: Option<String>,
    /// Variables every process started here gets, as far as its trust
    /// allows (see [`Sandbox::set_spawn_env`]). They never enter this
    /// process's own environment.
    spawn_env: RwLock<BTreeMap<String, String>>,
    /// Refuse to start anything when policies can't be enforced (the cloud
    /// runner: there's no person at the keyboard to notice).
    strict: AtomicBool,
    home: Option<PathBuf>,
    /// tab's own data and config folders.
    data_dir: Option<PathBuf>,
    config_dir: Option<PathBuf>,
    /// Another tab's data and config folders on this computer, guarded
    /// like this one's: a dev build's agents can't change the installed
    /// app's files or read its tokens.
    others: Vec<PathBuf>,
    /// Confine on every OS, not only macOS: tests check policies with
    /// [`Policy::allows`] wherever they run.
    confine_anywhere: bool,
}

static CURRENT: OnceLock<Arc<Sandbox>> = OnceLock::new();

/// Makes this process's memory and environment unreadable to other
/// processes of the same user (Linux: not dumpable, so `/proc/<pid>/environ`
/// and `/proc/<pid>/mem` belong to root and `ptrace` is refused). Call first
/// thing in `main`. Elsewhere it does nothing: on macOS the sandbox profile
/// keeps agents from reading other processes' environments instead. Dev
/// builds stay debuggable unless `force`.
pub fn harden_process(force: bool) {
    if cfg!(debug_assertions) && !force {
        return;
    }
    #[cfg(target_os = "linux")]
    {
        // SAFETY: prctl with PR_SET_DUMPABLE takes plain integers.
        let r = unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) };
        if r != 0 {
            tracing::warn!(error = %std::io::Error::last_os_error(), "couldn't make tab non-dumpable");
        }
    }
}

/// Sets the app's sandbox. Call once at startup.
pub fn install(s: Arc<Sandbox>) {
    let _ = CURRENT.set(s);
}

/// The app's sandbox, or one that knows nothing about tab's folders when
/// [`install`] wasn't called (tests, examples).
pub fn current() -> Arc<Sandbox> {
    CURRENT
        .get_or_init(|| Arc::new(Sandbox::new(SandboxConfig::default(), None, None)))
        .clone()
}

pub(crate) fn home() -> Option<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

/// Resolves links (`/tmp` → `/private/tmp` on macOS) so rules match the
/// paths the kernel sees. Paths that don't exist yet stay as they are.
pub(crate) fn real(p: &Path) -> PathBuf {
    match std::fs::canonicalize(p) {
        Ok(r) => {
            let s = r.display().to_string();
            s.strip_prefix(r"\\?\").map(PathBuf::from).unwrap_or(r)
        }
        Err(_) => p.to_path_buf(),
    }
}

/// A checkout's other git worktrees, read from `.git/worktrees/*/gitdir`
/// (each names the worktree's own `.git` file).
pub(crate) fn worktrees(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root.join(".git").join("worktrees")) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|e| std::fs::read_to_string(e.path().join("gitdir")).ok())
        .filter_map(|g| Path::new(g.trim()).parent().map(real))
        .collect()
}

/// What in Claude Code's folder belongs to other conversations: their
/// transcripts, the prompts typed into them, backups of files they edited.
const OTHER_CONVERSATIONS: &[&str] = &[
    "projects",
    "history.jsonl",
    "file-history",
    "todos",
    "debug",
    "session-env",
    "paste-cache",
];

/// How Claude Code names a working folder's transcript folder.
pub(crate) fn claude_project_slug(cwd: &Path) -> String {
    cwd.display()
        .to_string()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// When the home folder is confined: Claude Code (folder `dir`) may use
/// its folder and settings, but of the conversations kept there only
/// conversation `id` (and the working folder's memory).
pub(crate) fn own_claude_conversation(
    policy: &mut Policy,
    dir: &Path,
    home: &Path,
    cwd: &Path,
    id: &str,
) {
    if policy.confine.is_empty() {
        return;
    }
    let dir = real(dir);
    policy.open.push(dir.clone());
    // Its settings file and the copies it writes beside it.
    policy.open.push(real(home).join(".claude.json*"));
    policy
        .hide
        .extend(OTHER_CONVERSATIONS.iter().map(|o| dir.join(o)));
    let mut slugs = vec![claude_project_slug(cwd), claude_project_slug(&real(cwd))];
    slugs.dedup();
    for slug in slugs {
        let project = dir.join("projects").join(&slug);
        // Made here: inside the hidden folder the agent couldn't.
        let _ = std::fs::create_dir_all(&project);
        policy.reveal_rw.extend([
            project.join(format!("{id}.jsonl")),
            project.join(id),
            project.join("memory"),
        ]);
    }
    policy.reveal_rw.extend([
        dir.join("file-history").join(id),
        dir.join("session-env").join(id),
        dir.join("debug").join(format!("{id}.txt")),
        dir.join("todos").join(format!("{id}*")),
    ]);
}

/// Read or write, for [`Policy::allows`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Read,
    Write,
}

/// Whether one of `entries` covers `p`: the path itself or anything in it,
/// or, for an entry ending in `*`, anything starting with it.
fn covers(entries: &[PathBuf], p: &Path) -> bool {
    entries
        .iter()
        .any(|e| match e.to_str().and_then(|s| s.strip_suffix('*')) {
            Some(prefix) => p.to_str().is_some_and(|s| s.starts_with(prefix)),
            None => e.as_os_str() == "/" || p.starts_with(e),
        })
}

impl Policy {
    /// What the Seatbelt profile ([`seatbelt::profile`]) decides for `p`:
    /// the same rules in the same order, the last that matches winning.
    /// Tests check private places against it on every OS, and check it
    /// against the real sandbox where one starts.
    pub fn allows(&self, p: &Path, access: Access) -> bool {
        let mut ok = true;
        match access {
            Access::Read => {
                if covers(&self.confine, p) {
                    ok = false;
                }
                if covers(&self.readable, p) || covers(&self.open, p) || covers(&self.writable, p) {
                    ok = true;
                }
                if covers(&self.hide, p) {
                    ok = false;
                }
                if covers(&self.reveal, p) || covers(&self.reveal_rw, p) {
                    ok = true;
                }
                if self.block_exec.iter().any(|b| b == p) {
                    ok = false;
                }
            }
            Access::Write => {
                if covers(&self.confine, p) {
                    ok = false;
                }
                if covers(&self.open, p) {
                    ok = true;
                }
                if covers(&self.protect, p) {
                    ok = false;
                }
                if covers(&self.writable, p) {
                    ok = true;
                }
                if covers(&self.hide, p) {
                    ok = false;
                }
                if covers(&self.reveal_rw, p) {
                    ok = true;
                }
            }
        }
        ok
    }
}

/// The checkout a dev build of tab runs from. None in release builds.
pub fn dev_checkout() -> Option<PathBuf> {
    if !cfg!(debug_assertions) {
        return None;
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let root = real(&root);
    root.join(".git").exists().then_some(root)
}

/// Places that hold credentials, relative to home. Hidden from every
/// sandboxed process.
const SECRETS: &[&str] = &[
    ".aws",
    ".azure",
    ".config/gcloud",
    ".kube",
    ".docker/config.json",
    ".gnupg",
    ".password-store",
    ".local/share/keyrings",
    ".vault-token",
    ".terraform.d/credentials.tfrc.json",
    ".config/op",
    // Browser profiles: cookies and saved passwords.
    ".mozilla",
    ".config/google-chrome",
    ".config/chromium",
    ".config/BraveSoftware",
    ".config/microsoft-edge",
    "Library/Application Support/Google/Chrome",
    "Library/Application Support/Firefox",
    "Library/Application Support/BraveSoftware",
    "Library/Application Support/Microsoft Edge",
    "Library/Cookies",
    "Library/Safari",
];

/// tab's own secrets in its config folder: every connection's settings,
/// and the person's tab cloud sign-in (a Supabase refresh token).
const CONFIG_SECRETS: &[&str] = &["config.json", "account.json"];

/// tab's own secrets in its data folder: the database (connection tokens),
/// the local hub's copy of every synced record, and the hub's TLS key.
const DATA_SECRETS: &[&str] = &[
    "tab.db",
    "tab.db-wal",
    "tab.db-shm",
    "mcp-token",
    "hub.db",
    "hub.db-wal",
    "hub.db-shm",
    "hub-tls",
];

/// Hidden files that tools (or tab itself) may create while a process runs.
/// bubblewrap can only cover a path that exists when the process starts, so
/// these are made empty (readable by this user only) first. Each is fine to
/// find empty: git, curl and tab all read an empty one as "nothing stored".
const MADE_BEFORE_HIDING: &[&str] = &[".git-credentials", ".netrc", "account.json", "mcp-token"];

/// Hidden folders made (empty, this user only) before a process starts, for
/// the same reason.
const DIRS_MADE_BEFORE_HIDING: &[&str] = &["hub-tls"];

/// Credentials a person's own chat may use (to push and open PRs), but code
/// nobody here wrote may not.
const GIT_SECRETS: &[&str] = &[
    ".ssh",
    ".config/gh",
    ".git-credentials",
    ".netrc",
    ".npmrc",
    ".cargo/credentials",
    ".cargo/credentials.toml",
    ".pypirc",
];

/// What tools need to read under the home folder when it's confined:
/// toolchains, version managers and the shell and git setup they start
/// from. Read-only; credentials among them stay hidden by `SECRETS` and
/// `GIT_SECRETS`.
const TOOLCHAINS: &[&str] = &[
    ".cargo",
    ".nvm",
    ".volta",
    ".fnm",
    ".bun",
    ".deno",
    ".pyenv",
    ".rbenv",
    ".asdf",
    ".sdkman",
    ".gradle",
    ".m2",
    ".yarn",
    ".npm-global",
    ".local/bin",
    ".local/share/claude",
    ".local/share/mise",
    ".local/share/uv",
    ".local/state/mise",
    "go/bin",
    "Library/pnpm",
    "Library/Python",
    ".config/git",
    ".config/gh",
    ".config/pnpm",
    ".config/yarn",
    ".config/mise",
    ".config/fish",
    ".config/starship.toml",
    ".oh-my-zsh",
    ".zshrc",
    ".zshenv",
    ".zprofile",
    ".zlogin",
    ".bashrc",
    ".bash_profile",
    ".bash_login",
    ".profile",
    ".gitconfig",
    ".gitignore_global",
    ".editorconfig",
    ".npmrc",
    ".ssh",
    ".docker",
    // The login keychain's file: `security` (which Claude Code signs in
    // with) opens it in-process. Its items stay encrypted; macOS asks
    // before handing any out.
    "Library/Keychains",
];

/// Folders confined besides the home folder: other people's homes and
/// external drives.
const CONFINED_ROOTS: &[&str] = if cfg!(target_os = "linux") {
    &["/home", "/mnt", "/media"]
} else if cfg!(target_os = "macos") {
    &["/Users", "/Volumes"]
} else {
    &[]
};

/// Where the other agent harnesses tab runs keep their sign-in, settings
/// and conversations: usable whole, unlike Claude Code's (see
/// [`own_claude_conversation`]), until each gets the same split.
const HARNESSES: &[&str] = &[
    ".codex",
    ".config/opencode",
    ".local/share/opencode",
    ".local/state/opencode",
    ".pi",
];

/// Toolchain folders that are also written to: installs a project asks
/// for (`rust-toolchain.toml`), and global caches.
const TOOLCHAINS_RW: &[&str] = &[".rustup"];

/// Files that run on every new shell or login: never writable, so nothing
/// can leave a command behind for later.
const STARTUP: &[&str] = &[
    ".bashrc",
    ".bash_profile",
    ".bash_login",
    ".profile",
    ".zshrc",
    ".zshenv",
    ".zprofile",
    ".zlogin",
    ".config/fish",
    ".ssh",
    ".gitconfig",
    ".config/git",
    "Library/LaunchAgents",
    ".config/autostart",
    ".config/systemd",
];

/// Caches build tools write to, so isolated processes can still fetch and
/// build dependencies.
const CACHES: &[&str] = &[
    ".cache",
    ".cargo/registry",
    ".cargo/git",
    "go/pkg/mod",
    ".npm",
    ".local/share/pnpm",
    ".bun/install/cache",
    "Library/Caches",
];

impl Sandbox {
    pub fn new(cfg: SandboxConfig, data_dir: Option<PathBuf>, config_dir: Option<PathBuf>) -> Self {
        let (backend, detected) = detect();
        Self {
            cfg: RwLock::new(cfg),
            backend,
            detected,
            spawn_env: RwLock::new(BTreeMap::new()),
            strict: AtomicBool::new(false),
            home: home().map(|h| real(&h)),
            data_dir: data_dir.map(|d| real(&d)),
            config_dir: config_dir.map(|d| real(&d)),
            others: Vec::new(),
            confine_anywhere: false,
        }
    }

    /// Takes `home` as the home folder and confines it on any OS (tests).
    #[cfg(test)]
    pub(crate) fn confining_home(mut self, home: &Path) -> Self {
        self.home = Some(real(home));
        self.confine_anywhere = true;
        self
    }

    /// Also guards another tab's folders (see [`Sandbox::others`]).
    pub fn guarding(mut self, dirs: impl IntoIterator<Item = PathBuf>) -> Self {
        self.others.extend(dirs.into_iter().map(|d| real(&d)));
        self.others.sort();
        self.others.dedup();
        self
    }

    /// Sets the variables every process started from here on gets, on top
    /// of what's left of tab's own environment. Each still goes through the
    /// trust filter ([`env::keeps`]) and the policy's `drop_env`, so a model
    /// key reaches an agent and a GitHub token reaches only the person's own
    /// chats. Used by the cloud runner, which keeps the hub's secrets in
    /// memory instead of in its own environment, where any process of the
    /// same user could read them from `/proc`.
    pub fn set_spawn_env(&self, vars: BTreeMap<String, String>) {
        *self.spawn_env.write().unwrap() = vars;
    }

    /// Refuses to start a process when its policy can't be enforced here.
    pub fn require_enforcement(&self, on: bool) {
        self.strict.store(on, Ordering::SeqCst);
    }

    pub fn set_config(&self, cfg: SandboxConfig) {
        *self.cfg.write().unwrap() = cfg;
    }

    pub fn config(&self) -> SandboxConfig {
        self.cfg.read().unwrap().clone()
    }

    pub fn backend(&self) -> Backend {
        self.backend
    }

    /// Whether file and network limits apply on this computer right now.
    pub fn enforced(&self) -> bool {
        self.config().enabled && self.backend.enforces()
    }

    pub async fn status(&self) -> Status {
        let enabled = self.config().enabled;
        let container = container::detect(&self.config().container_runtime).await;
        let (summary, fix) = match (enabled, self.backend) {
            (false, _) => (
                "Off. Agents and commands still start without tab's secrets, and stopping a chat ends everything it started.".to_string(),
                None,
            ),
            (true, Backend::Seatbelt) if self.confines() => (
                "On, with macOS's built-in sandbox. Agents only reach the chat's own folder, your code folders, toolchains and caches: the rest of your home folder, other chats and tab's data are off limits.".to_string(),
                None,
            ),
            (true, Backend::Seatbelt) => (
                "On, with macOS's built-in sandbox. Agents can't change tab's own files or your shell setup, or read cloud credentials and browser data.".to_string(),
                None,
            ),
            (true, Backend::Bubblewrap) if self.confines() => (
                "On, with bubblewrap. Agents only reach the chat's own folder, your code folders, toolchains and caches: the rest of your home folder, other chats and tab's data are off limits.".to_string(),
                None,
            ),
            (true, Backend::Bubblewrap) => (
                "On, with bubblewrap. Agents can't change tab's own files or your shell setup, or read cloud credentials and browser data.".to_string(),
                None,
            ),
            (true, Backend::Inherited) => (
                "tab is running inside a sandbox already, and macOS can't start another inside it, so agents and commands run under that sandbox's limits instead of tab's. Review chats and workflow commands don't get their own stricter limits here.".to_string(),
                Some("Run tab outside the sandbox (from Finder, or a terminal that isn't sandboxed) to get tab's own limits.".to_string()),
            ),
            (true, Backend::Unavailable) if cfg!(target_os = "macos") => (
                "macOS's sandbox didn't start here, so file limits aren't enforced.".to_string(),
                self.detected.clone(),
            ),
            (true, Backend::Unavailable) if cfg!(windows) => (
                "Windows has no sandbox tab can use yet, so file limits aren't enforced. Agents still start without tab's secrets, Claude Code is told not to touch protected folders, and stopping a chat ends everything it started.".to_string(),
                Some("Workflow commands that name a container image run isolated in Docker or Podman.".to_string()),
            ),
            (true, Backend::Unavailable) => (
                "bubblewrap isn't working here, so file and network limits aren't enforced.".to_string(),
                Some("Install bubblewrap (apt install bubblewrap, dnf install bubblewrap). On Ubuntu 24.04+, unprivileged user namespaces must be allowed for it.".to_string()),
            ),
        };
        Status {
            enabled,
            backend: self.backend,
            enforced: enabled && self.backend.enforces(),
            summary,
            fix,
            container,
        }
    }

    fn under_home(&self, rel: &[&str]) -> Vec<PathBuf> {
        let Some(h) = &self.home else {
            return Vec::new();
        };
        rel.iter().map(|r| h.join(r)).collect()
    }

    /// Paths from Settings, `~/` meaning this sandbox's home folder.
    fn user_paths(&self, list: &[String]) -> Vec<PathBuf> {
        list.iter()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| {
                let p = match (s.strip_prefix("~/"), &self.home) {
                    (Some(rest), Some(h)) => h.join(rest),
                    _ => PathBuf::from(s),
                };
                real(&p)
            })
            .collect()
    }

    /// tab's own files: its database and config hold every connection's
    /// token, `account.json` the person's tab cloud sign-in, `hub.db` a copy
    /// of every synced record (other chats' transcripts included), and
    /// `hub-tls` the local hub's private key.
    pub(crate) fn tab_secrets(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        // Just the files: on macOS and Windows the config and data folders
        // are the same, and agents read their chat's files from there.
        if let Some(c) = &self.config_dir {
            out.extend(CONFIG_SECRETS.iter().map(|f| c.join(f)));
        }
        if let Some(d) = &self.data_dir {
            out.extend(DATA_SECRETS.iter().map(|f| d.join(f)));
        }
        for d in &self.others {
            out.extend(CONFIG_SECRETS.iter().chain(DATA_SECRETS).map(|f| d.join(f)));
        }
        out.sort();
        out.dedup();
        out
    }

    /// Whether policies lock the home folder down to what's needed (see
    /// [`SandboxConfig::confine`]): macOS (Seatbelt) and Linux (bubblewrap,
    /// the cloud runner included). Not Windows, which enforces nothing.
    pub fn confines(&self) -> bool {
        let c = self.config();
        let os = cfg!(any(target_os = "macos", target_os = "linux"));
        c.enabled && c.confine && (os || self.confine_anywhere)
    }

    /// Locks the home folder, other people's and external drives, leaving
    /// tools, caches, the chat's attachments and, in the person's own
    /// chats, their code folders. The chat's
    /// own folder is added when it starts ([`Sandbox::command`]), its
    /// project's repos by `AgentHub::policy`.
    fn confine(&self, p: &mut Policy) {
        if !self.confines() {
            return;
        }
        p.confine
            .extend(CONFINED_ROOTS.iter().map(|r| real(Path::new(r))));
        if let Some(h) = self
            .home
            .clone()
            .filter(|h| !p.confine.iter().any(|r| h.starts_with(r)))
        {
            p.confine.push(h);
        }
        // tab's own folders where they aren't in the home folder: the cloud
        // runner's `/data`, holding every job's checkout and session.
        let tabs = self
            .data_dir
            .iter()
            .chain(&self.config_dir)
            .chain(&self.others);
        for d in tabs.cloned().collect::<Vec<_>>() {
            if !p.confine.iter().any(|r| d.starts_with(r)) {
                p.confine.push(d);
            }
        }
        p.readable.extend(self.under_home(TOOLCHAINS));
        p.readable.extend(dev_checkout());
        if let Some(d) = &self.data_dir {
            p.readable.push(d.join("attachments"));
        }
        p.open.extend(self.under_home(TOOLCHAINS_RW));
        p.open.extend(self.under_home(HARNESSES));
        for var in [
            "CODEX_HOME",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "XDG_STATE_HOME",
        ] {
            if let Some(d) = std::env::var_os(var).filter(|d| !d.is_empty()) {
                let d = real(Path::new(&d));
                // Only a harness's own folder, never a whole XDG root.
                if var == "CODEX_HOME" {
                    p.open.push(d);
                } else {
                    p.open.push(d.join("opencode"));
                }
            }
        }
        p.open.extend(self.under_home(CACHES));
        // Someone else's code (a review, a language server, a workflow
        // command) gets its own folder only, not the person's other code.
        if p.trust == Trust::Person {
            p.open.extend(self.user_paths(&self.config().folders));
        }
    }

    /// Lets a confined process into the folder it starts in and the folder
    /// its program lives in. Not when it starts in the home folder (or above
    /// it): that would open everything again.
    pub(crate) fn starting_in(&self, policy: &Policy, exe: &Path, cwd: &Path) -> Policy {
        let mut p = policy.clone();
        if p.confine.is_empty() {
            return p;
        }
        let cwd = real(cwd);
        let too_wide = self.home.as_ref().is_some_and(|h| h.starts_with(&cwd))
            || p.confine.iter().any(|r| r.starts_with(&cwd));
        if !too_wide {
            p.open.push(cwd);
        }
        for e in [exe.to_path_buf(), real(exe)] {
            if let Some(dir) = e.parent().filter(|d| d.components().count() > 1) {
                let dir = dir.to_path_buf();
                let wide = self.home.as_ref().is_some_and(|h| h.starts_with(&dir));
                if !wide && !p.readable.contains(&dir) {
                    p.readable.push(dir);
                }
            }
        }
        p
    }

    fn common(&self, trust: Trust) -> (Vec<PathBuf>, Vec<PathBuf>) {
        let cfg = self.config();
        let mut protect: Vec<PathBuf> = Vec::new();
        protect.extend(dev_checkout());
        protect.extend(self.data_dir.clone());
        protect.extend(self.config_dir.clone());
        protect.extend(self.others.iter().cloned());
        protect.extend(self.under_home(STARTUP));
        protect.extend(self.user_paths(&cfg.protected));
        let mut hide = self.under_home(SECRETS);
        hide.extend(self.tab_secrets());
        hide.extend(self.user_paths(&cfg.hidden));
        if trust != Trust::Person {
            hide.extend(self.under_home(GIT_SECRETS));
        }
        (protect, hide)
    }

    /// A chat the person drives, starting in `cwd`. Agents can still edit
    /// anything else, push with the person's git credentials and use the
    /// network; what's off limits is tab itself, startup files and stored
    /// credentials.
    pub fn chat_policy(&self) -> Policy {
        let (protect, hide) = self.common(Trust::Person);
        let mut writable = Vec::new();
        // `git worktree add` from tab's checkout writes into its .git.
        if let Some(root) = dev_checkout() {
            writable.push(root.join(".git"));
        }
        let mut p = Policy {
            protect,
            writable,
            hide,
            reveal: Vec::new(),
            reveal_rw: Vec::new(),
            confine: Vec::new(),
            open: Vec::new(),
            readable: Vec::new(),
            block_exec: Vec::new(),
            drop_env: Vec::new(),
            brokered: false,
            network: Network::Any,
            trust: Trust::Person,
        };
        self.confine(&mut p);
        p
    }

    /// An agent chat about someone else's pull request, in its checkout.
    /// It may change the checkout and nothing else of the person's code,
    /// and can't read git or SSH credentials.
    pub fn review_policy(&self, checkout: &Path, workdir: &Path) -> Policy {
        let (mut protect, hide) = self.common(Trust::Agent);
        protect.push(real(workdir));
        let mut p = Policy {
            protect,
            writable: vec![real(checkout)],
            hide,
            reveal: Vec::new(),
            reveal_rw: Vec::new(),
            confine: Vec::new(),
            open: Vec::new(),
            readable: Vec::new(),
            block_exec: Vec::new(),
            drop_env: Vec::new(),
            brokered: false,
            network: Network::Any,
            trust: Trust::Agent,
        };
        self.confine(&mut p);
        p
    }

    /// A language server or command: read anything not hidden, write only
    /// to `writable`, temp and build caches.
    pub fn isolated_policy(&self, writable: &[PathBuf], network: Network) -> Policy {
        let (_, hide) = self.common(Trust::Untrusted);
        let mut w: Vec<PathBuf> = writable.iter().map(|p| real(p)).collect();
        w.extend(self.under_home(CACHES));
        w.extend(temp_dirs());
        let mut p = Policy {
            protect: vec![PathBuf::from("/")],
            writable: w,
            hide,
            reveal: Vec::new(),
            reveal_rw: Vec::new(),
            confine: Vec::new(),
            open: Vec::new(),
            readable: Vec::new(),
            block_exec: Vec::new(),
            drop_env: Vec::new(),
            brokered: false,
            network,
            trust: Trust::Untrusted,
        };
        self.confine(&mut p);
        p
    }

    /// A chat whose `gh` and git network commands go through tab (see
    /// [`broker`]): the agent can't start the real CLIs that need a token or
    /// read where they keep it, and gets no GitHub token in its environment.
    pub fn brokered(&self, mut p: Policy) -> Policy {
        if !self.config().enabled {
            return p;
        }
        p.brokered = true;
        for b in broker::all() {
            if b.cli.default == tab_domain::CliEffect::Local {
                continue;
            }
            if let Some(exe) = b.real() {
                let resolved = real(&exe);
                if resolved != exe {
                    p.block_exec.push(resolved);
                }
                p.block_exec.push(exe);
            }
        }
        // git's own credential stores: GitHub goes through tab, and other
        // hosts get their credentials through tab's helper (see
        // `broker::credential`), so nothing in the chat reads them directly.
        p.block_exec.extend(credential_helpers());
        p.hide.extend(self.under_home(&[
            ".config/gh",
            "AppData/Roaming/GitHub CLI",
            ".git-credentials",
            ".config/git/credentials",
            ".cache/git/credential",
            ".git-credential-cache",
        ]));
        if !self.config().ssh_in_chats {
            p.hide.extend(self.under_home(&[".ssh"]));
            p.drop_env.push("SSH_AUTH_SOCK".into());
        }
        p.drop_env
            .extend(env::GITHUB_KEYS.iter().map(|k| k.to_string()));
        p
    }

    /// Builds the command for `exe args` in `cwd` under `policy`.
    pub async fn command(
        &self,
        exe: &Path,
        args: &[OsString],
        cwd: &Path,
        policy: &Policy,
    ) -> anyhow::Result<Prepared> {
        let enabled = self.config().enabled;
        if self.strict.load(Ordering::SeqCst) && !(enabled && self.backend.enforces()) {
            anyhow::bail!(
                "tab's sandbox isn't working here ({:?}), and this runner starts nothing without it",
                self.backend
            );
        }
        // Under bubblewrap a domain list reaches the proxy through a Unix
        // socket and a bridge inside the sandbox; without the bridge the
        // process gets no network at all rather than all of it.
        let bridged = enabled
            && self.backend == Backend::Bubblewrap
            && matches!(policy.network, Network::Domains(_))
            && bridge::helper().is_some();
        let proxy = match (&policy.network, enabled) {
            (Network::Domains(d), true) => Some(proxy::start_on(d.clone(), bridged).await?),
            _ => None,
        };
        let port = proxy.as_ref().map(|p| p.port);
        let policy = &self.starting_in(policy, exe, cwd);
        if enabled && self.backend == Backend::Bubblewrap {
            prepare_hidden(policy);
        }
        let (mut cmd, enforced) = match (enabled, self.backend) {
            (true, Backend::Seatbelt) => {
                let profile = seatbelt::profile(policy, port, seatbelt_procargs());
                let mut c = tokio::process::Command::new("/usr/bin/sandbox-exec");
                c.arg("-p").arg(profile).arg(exe).args(args);
                (c, true)
            }
            (true, Backend::Bubblewrap) => {
                let sock = proxy.as_ref().and_then(|p| p.socket.clone());
                let x = bwrap::Extras {
                    pid: bwrap_pid(),
                    net_dir: sock.as_deref().and_then(Path::parent),
                };
                let mut c = tokio::process::Command::new("bwrap");
                c.args(bwrap::args(policy, cwd, x)).arg("--");
                match (sock, port, bridge::helper()) {
                    (Some(sock), Some(port), Some(helper)) => {
                        c.arg(helper)
                            .arg(bridge::SUBCOMMAND)
                            .arg(sock)
                            .arg(port.to_string())
                            .arg(exe)
                            .args(args);
                    }
                    _ => {
                        c.arg(exe).args(args);
                    }
                }
                (c, true)
            }
            _ => {
                let mut c = tokio::process::Command::new(exe);
                c.args(args);
                (c, false)
            }
        };
        cmd.current_dir(cwd);
        env::scrub(&mut cmd, policy.trust);
        for (k, v) in self.spawn_env.read().unwrap().iter() {
            if env::keeps(k, policy.trust) {
                cmd.env(k, v);
            }
        }
        for k in &policy.drop_env {
            cmd.env_remove(k);
        }
        if let Some(port) = port {
            proxy::point_at(&mut cmd, port);
        }
        #[cfg(windows)]
        {
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        cmd.kill_on_drop(true);
        Ok(Prepared {
            cmd,
            enforced,
            proxy,
        })
    }

    /// Claude Code's own permission rules for a policy. They apply on every
    /// OS, Windows included, to Claude's file tools: edits to protected
    /// folders and reads of hidden ones are refused before they happen.
    /// They never change the person's permission mode.
    ///
    /// Where the sandbox is enforced, shell commands run without asking: the
    /// sandbox already keeps them off tab, the person's shell setup and
    /// credentials, and tab asks before anything risky on GitHub. Where it
    /// isn't (Windows), only `gh` and `git` do, since tab checks those itself.
    pub fn claude_settings(&self, policy: &Policy) -> serde_json::Value {
        let rule = |tool: &str, p: &Path| {
            let s = claude_path(p);
            if p.is_file() {
                vec![format!("{tool}({s})")]
            } else if p.is_dir() {
                vec![format!("{tool}({s}/**)")]
            } else {
                vec![format!("{tool}({s})"), format!("{tool}({s}/**)")]
            }
        };
        let mut deny = Vec::new();
        if self.config().enabled {
            for p in policy.protect.iter().filter(|p| p.as_os_str() != "/") {
                if policy.writable.iter().any(|w| p.starts_with(w)) {
                    continue;
                }
                // Claude Code's `Edit` rules cover every file write.
                deny.extend(rule("Edit", p));
            }
            // A hidden folder with a way back in (Claude Code's own
            // transcript, the chat's memory) is left to the sandbox: Claude
            // Code's rules can't make the exception.
            for p in policy
                .hide
                .iter()
                .filter(|h| !policy.reveal_rw.iter().any(|r| r.starts_with(h)))
            {
                deny.extend(rule("Read", p));
            }
        }
        // Shell commands run without asking only where the sandbox holds
        // their network to the policy too: under bubblewrap a domain list
        // without the bridge means no network, so it still holds.
        let network_holds = match &policy.network {
            Network::Any => true,
            Network::None => true,
            Network::Domains(_) => self.backend == Backend::Seatbelt || bridge::helper().is_some(),
        };
        let allow: Vec<&str> = if self.enforced() && network_holds {
            vec!["Bash"]
        } else if policy.brokered {
            vec!["Bash(gh:*)", "Bash(git:*)"]
        } else {
            Vec::new()
        };
        // No CLAUDE_CODE_SUBPROCESS_ENV_SCRUB: it switches Claude Code out
        // of auto mode, so every command asks. tab scrubs the environment
        // itself before Claude Code starts (see `env::scrub`).
        serde_json::json!({ "permissions": { "allow": allow, "deny": deny } })
    }
}

/// A path the way Claude Code's permission rules spell an absolute one:
/// `//home/me/x`, and on Windows `//c/Users/me/x` for `C:\Users\me\x`.
fn claude_path(p: &Path) -> String {
    let s = p.display().to_string().replace('\\', "/");
    let b = s.as_bytes();
    if b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic() {
        return format!("//{}{}", (b[0] as char).to_ascii_lowercase(), &s[2..]);
    }
    if s.starts_with('/') {
        format!("/{s}")
    } else {
        format!("//{s}")
    }
}

/// git's credential helper programs, wherever this computer has them.
fn credential_helpers() -> Vec<PathBuf> {
    static FOUND: OnceLock<Vec<PathBuf>> = OnceLock::new();
    FOUND
        .get_or_init(|| {
            const NAMES: &[&str] = &[
                "git-credential-osxkeychain",
                "git-credential-libsecret",
                "git-credential-gnome-keyring",
                "git-credential-manager",
                "git-credential-manager-core",
                "git-credential-wincred",
            ];
            let exec_path = std::process::Command::new("git")
                .arg("--exec-path")
                .stdin(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .output()
                .ok()
                .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()));
            let mut out = Vec::new();
            for n in NAMES {
                for p in exec_path
                    .iter()
                    .map(|d| d.join(n))
                    .chain(which::which(n).ok())
                {
                    if p.is_file() {
                        let r = real(&p);
                        if r != p {
                            out.push(r);
                        }
                        out.push(p);
                    }
                }
            }
            out.sort();
            out.dedup();
            out
        })
        .clone()
}

fn temp_dirs() -> Vec<PathBuf> {
    let mut v = vec![real(&std::env::temp_dir())];
    if cfg!(unix) {
        v.push(real(Path::new("/tmp")));
        v.push(PathBuf::from("/dev"));
    }
    v.dedup();
    v
}

/// Which sandbox works here, and what went wrong when none does. Checked
/// once: Seatbelt is part of macOS, and bubblewrap is tried for real, since
/// distributions can block the user namespaces it needs.
fn detect() -> (Backend, Option<String>) {
    if cfg!(target_os = "macos") && Path::new("/usr/bin/sandbox-exec").exists() {
        let status = std::process::Command::new("/usr/bin/sandbox-exec")
            .args(["-p", "(version 1)(allow default)", "/usr/bin/true"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        return match status {
            Ok(s) if s.success() => (Backend::Seatbelt, None),
            // sandbox_apply refuses (EX_OSERR, 71) inside another sandbox.
            Ok(s) if s.code() == Some(71) => (Backend::Inherited, None),
            // Anything else is no sandbox at all, and is said so: never
            // taken as "the outer sandbox applies".
            Ok(s) => (
                Backend::Unavailable,
                Some(format!("sandbox-exec answered {s}.")),
            ),
            Err(e) => (
                Backend::Unavailable,
                Some(format!("sandbox-exec didn't start: {e}.")),
            ),
        };
    }
    if cfg!(target_os = "linux") {
        if bwrap_works(&["--dev-bind", "/", "/", "--", "true"]) {
            return (Backend::Bubblewrap, None);
        }
        return (
            Backend::Unavailable,
            Some("bubblewrap didn't start a sandbox here.".into()),
        );
    }
    (Backend::Unavailable, None)
}

fn bwrap_works(args: &[&str]) -> bool {
    std::process::Command::new("bwrap")
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Whether bubblewrap can give a process its own PID namespace and `/proc`
/// here (some container runtimes refuse to mount `/proc`).
fn bwrap_pid() -> bool {
    static OK: OnceLock<bool> = OnceLock::new();
    *OK.get_or_init(|| {
        bwrap_works(&[
            "--dev-bind",
            "/",
            "/",
            "--unshare-pid",
            "--proc",
            "/proc",
            "--",
            "true",
        ])
    })
}

/// Whether this macOS's Seatbelt takes the rule that hides other processes'
/// environments; checked once, so a macOS that doesn't know the filter
/// still gets the rest of the profile.
fn seatbelt_procargs() -> bool {
    static OK: OnceLock<bool> = OnceLock::new();
    *OK.get_or_init(|| {
        let profile = format!("(version 1)(allow default){}", seatbelt::PROCARGS_RULE);
        std::process::Command::new("/usr/bin/sandbox-exec")
            .args(["-p", &profile, "/usr/bin/true"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    })
}

/// bubblewrap covers a hidden path only if it exists when the process
/// starts, so a credential file a tool writes mid-chat would be readable.
/// The ones that are safe to find empty are made first (readable by this
/// user only).
fn prepare_hidden(policy: &Policy) {
    for p in &policy.hide {
        if p.exists() {
            continue;
        }
        let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !p.parent().is_some_and(Path::is_dir) {
            continue;
        }
        if MADE_BEFORE_HIDING.contains(&name) {
            let mut o = std::fs::OpenOptions::new();
            o.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                o.mode(0o600);
            }
            let _ = o.open(p);
        } else if DIRS_MADE_BEFORE_HIDING.contains(&name) {
            let mut b = std::fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                b.mode(0o700);
            }
            let _ = b.create(p);
        }
    }
}

#[cfg(test)]
mod tests;
