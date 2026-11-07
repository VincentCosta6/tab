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
//! | Linux   | bubblewrap (`bwrap`)          | none via a network namespace; domain lists go through tab's proxy but aren't enforced |
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

pub mod broker;
mod bwrap;
pub mod container;
pub mod env;
pub mod proxy;
mod seatbelt;
pub mod tree;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
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
    /// Whether chats may use the person's SSH keys. On by default, since
    /// people push to other hosts over SSH; off keeps `~/.ssh` and the SSH
    /// agent out of chats, so GitHub is only reachable through tab.
    #[serde(default = "yes")]
    pub ssh_in_chats: bool,
}

fn yes() -> bool {
    true
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
    home: Option<PathBuf>,
    /// tab's own data and config folders.
    data_dir: Option<PathBuf>,
    config_dir: Option<PathBuf>,
    /// Another tab's data and config folders on this computer, guarded
    /// like this one's: a dev build's agents can't change the installed
    /// app's files or read its tokens.
    others: Vec<PathBuf>,
}

static CURRENT: OnceLock<Arc<Sandbox>> = OnceLock::new();

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
        Self {
            cfg: RwLock::new(cfg),
            backend: detect(),
            home: home().map(|h| real(&h)),
            data_dir: data_dir.map(|d| real(&d)),
            config_dir: config_dir.map(|d| real(&d)),
            others: Vec::new(),
        }
    }

    /// Also guards another tab's folders (see [`Sandbox::others`]).
    pub fn guarding(mut self, dirs: impl IntoIterator<Item = PathBuf>) -> Self {
        self.others.extend(dirs.into_iter().map(|d| real(&d)));
        self.others.sort();
        self.others.dedup();
        self
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
            (true, Backend::Seatbelt) => (
                "On, with macOS's built-in sandbox. Agents can't change tab's own files or your shell setup, or read cloud credentials and browser data.".to_string(),
                None,
            ),
            (true, Backend::Bubblewrap) => (
                "On, with bubblewrap. Agents can't change tab's own files or your shell setup, or read cloud credentials and browser data. Network allowlists aren't enforced on Linux yet.".to_string(),
                None,
            ),
            (true, Backend::Inherited) => (
                "tab is running inside a sandbox already, and macOS can't start another inside it, so agents and commands run under that sandbox's limits instead of tab's.".to_string(),
                None,
            ),
            (true, Backend::Unavailable) if cfg!(windows) => (
                "Windows has no sandbox tab can use yet, so file limits aren't enforced. Agents still start without tab's secrets, Claude Code is told not to touch protected folders, and stopping a chat ends everything it started.".to_string(),
                Some("Workflow commands that name a container image run isolated in Docker or Podman.".to_string()),
            ),
            (true, Backend::Unavailable) => (
                "bubblewrap isn't working here, so file limits aren't enforced.".to_string(),
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

    fn user_paths(list: &[String]) -> Vec<PathBuf> {
        list.iter()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| {
                let p = match (s.strip_prefix("~/"), home()) {
                    (Some(rest), Some(h)) => h.join(rest),
                    _ => PathBuf::from(s),
                };
                real(&p)
            })
            .collect()
    }

    /// tab's own files: its database and config hold every connection's
    /// token.
    fn tab_secrets(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        // Just the file: on macOS and Windows the config and data folders
        // are the same, and agents read their chat's files from there.
        if let Some(c) = &self.config_dir {
            out.push(c.join("config.json"));
        }
        if let Some(d) = &self.data_dir {
            for f in ["tab.db", "tab.db-wal", "tab.db-shm", "mcp-token"] {
                out.push(d.join(f));
            }
        }
        for d in &self.others {
            for f in [
                "config.json",
                "tab.db",
                "tab.db-wal",
                "tab.db-shm",
                "mcp-token",
            ] {
                out.push(d.join(f));
            }
        }
        out
    }

    fn common(&self, trust: Trust) -> (Vec<PathBuf>, Vec<PathBuf>) {
        let cfg = self.config();
        let mut protect: Vec<PathBuf> = Vec::new();
        protect.extend(dev_checkout());
        protect.extend(self.data_dir.clone());
        protect.extend(self.config_dir.clone());
        protect.extend(self.others.iter().cloned());
        protect.extend(self.under_home(STARTUP));
        protect.extend(Self::user_paths(&cfg.protected));
        let mut hide = self.under_home(SECRETS);
        hide.extend(self.tab_secrets());
        hide.extend(Self::user_paths(&cfg.hidden));
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
        Policy {
            protect,
            writable,
            hide,
            reveal: Vec::new(),
            block_exec: Vec::new(),
            drop_env: Vec::new(),
            brokered: false,
            network: Network::Any,
            trust: Trust::Person,
        }
    }

    /// An agent chat about someone else's pull request, in its checkout.
    /// It may change the checkout and nothing else of the person's code,
    /// and can't read git or SSH credentials.
    pub fn review_policy(&self, checkout: &Path, workdir: &Path) -> Policy {
        let (mut protect, hide) = self.common(Trust::Agent);
        protect.push(real(workdir));
        Policy {
            protect,
            writable: vec![real(checkout)],
            hide,
            reveal: Vec::new(),
            block_exec: Vec::new(),
            drop_env: Vec::new(),
            brokered: false,
            network: Network::Any,
            trust: Trust::Agent,
        }
    }

    /// A language server or command: read anything not hidden, write only
    /// to `writable`, temp and build caches.
    pub fn isolated_policy(&self, writable: &[PathBuf], network: Network) -> Policy {
        let (_, hide) = self.common(Trust::Untrusted);
        let mut w: Vec<PathBuf> = writable.iter().map(|p| real(p)).collect();
        w.extend(self.under_home(CACHES));
        w.extend(temp_dirs());
        Policy {
            protect: vec![PathBuf::from("/")],
            writable: w,
            hide,
            reveal: Vec::new(),
            block_exec: Vec::new(),
            drop_env: Vec::new(),
            brokered: false,
            network,
            trust: Trust::Untrusted,
        }
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
        let proxy = match (&policy.network, enabled) {
            (Network::Domains(d), true) => Some(proxy::start(d.clone()).await?),
            _ => None,
        };
        let port = proxy.as_ref().map(|p| p.port);
        let (mut cmd, enforced) = match (enabled, self.backend) {
            (true, Backend::Seatbelt) => {
                let profile = seatbelt::profile(policy, port);
                let mut c = tokio::process::Command::new("/usr/bin/sandbox-exec");
                c.arg("-p").arg(profile).arg(exe).args(args);
                (c, true)
            }
            (true, Backend::Bubblewrap) => {
                let mut c = tokio::process::Command::new("bwrap");
                c.args(bwrap::args(policy, cwd))
                    .arg("--")
                    .arg(exe)
                    .args(args);
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
            for p in &policy.hide {
                deny.extend(rule("Read", p));
            }
        }
        let allow: Vec<&str> = if self.enforced() {
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

/// Which sandbox works here. Checked once: Seatbelt is part of macOS, and
/// bubblewrap is tried for real, since distributions can block the user
/// namespaces it needs.
fn detect() -> Backend {
    if cfg!(target_os = "macos") && Path::new("/usr/bin/sandbox-exec").exists() {
        // Fails (exit 71) when this process is sandboxed already.
        let ok = std::process::Command::new("/usr/bin/sandbox-exec")
            .args(["-p", "(version 1)(allow default)", "/usr/bin/true"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        return if ok {
            Backend::Seatbelt
        } else {
            Backend::Inherited
        };
    }
    if cfg!(target_os = "linux") {
        let ok = std::process::Command::new("bwrap")
            .args(["--dev-bind", "/", "/", "--", "true"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if ok {
            return Backend::Bubblewrap;
        }
    }
    Backend::Unavailable
}

#[cfg(test)]
mod tests;
