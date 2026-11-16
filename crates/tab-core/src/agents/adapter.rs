//! ACP adapter programs (claude-agent-acp, codex-acp) that tab keeps itself,
//! so a harness's chats work without the person installing anything, and
//! only ever runs a build pinned in tab's own source:
//!
//! - An adapter with release builds (`acp.adapter.release`) is downloaded
//!   and checked against the sha256 in `harnesses.json`. It needs no Node.
//! - An npm adapter is installed with `npm ci` from
//!   `adapters/<command>.lock.json`, which pins every package by hash, with
//!   no install scripts. It runs on a Node that tab downloads the same way
//!   (`adapters/node.json`), so the person needn't have one.
//!
//! Both go in `<tools>/acp`, never globally, with the pinned version beside
//! them; a copy from an older pin is replaced. One on PATH is used only
//! when tab can't install its own here.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use sha2::{Digest, Sha256};

use super::catalog::{self, AcpAdapter, Pinned};
use super::{AgentKind, command, resolve, run_streaming};

static DIR: OnceLock<PathBuf> = OnceLock::new();

/// Where tab keeps adapters: `<dir>/<harness id>`, and Node in `<dir>/node`.
pub fn set_dir(dir: PathBuf) {
    let _ = DIR.set(dir);
}

/// How to start an adapter.
#[derive(Debug, Clone, PartialEq)]
pub struct Launch {
    pub program: PathBuf,
    /// Before the adapter's own arguments (its script, under Node).
    pub args: Vec<OsString>,
    /// Folders the sandbox must let it read: its install and Node's.
    pub readable: Vec<PathBuf>,
}

impl Launch {
    pub fn program(p: PathBuf) -> Self {
        Self {
            program: p,
            args: Vec::new(),
            readable: Vec::new(),
        }
    }
}

/// Node, pinned: the npm adapters run on it.
#[derive(serde::Deserialize)]
struct Node {
    version: String,
    builds: std::collections::BTreeMap<String, Pinned>,
}

fn node_pin() -> &'static Node {
    static NODE: OnceLock<Node> = OnceLock::new();
    NODE.get_or_init(|| {
        serde_json::from_str(include_str!("adapters/node.json")).expect("node.json is valid")
    })
}

/// The lockfile that pins an npm adapter and everything it pulls in.
fn lockfile(command: &str) -> Option<&'static str> {
    match command {
        "codex-acp" => Some(include_str!("adapters/codex-acp.lock.json")),
        _ => None,
    }
}

/// The adapter's program and what installs it, when `kind` runs through one.
fn spec(kind: AgentKind) -> Option<(&'static str, &'static AcpAdapter)> {
    let acp = catalog::harness(kind).acp.as_ref()?;
    Some((acp.command.as_deref()?, acp.adapter.as_ref()?))
}

/// This computer, as the pins name it.
fn target() -> Option<String> {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        "linux" => "linux",
        "windows" => "windows",
        _ => return None,
    };
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        _ => return None,
    };
    let musl = os == "linux" && Path::new("/etc/alpine-release").exists();
    Some(format!("{os}-{arch}{}", if musl { "-musl" } else { "" }))
}

/// How `kind`'s adapter is installed on this computer: a release build, or
/// npm on tab's Node. `None` when tab can't.
enum Plan {
    Release(&'static Pinned),
    Npm {
        pkg: &'static str,
        version: &'static str,
        lock: &'static str,
        node: &'static Pinned,
    },
}

fn plan(kind: AgentKind) -> Option<(Plan, String)> {
    let (cmd, a) = spec(kind)?;
    let t = target()?;
    if let Some(r) = &a.release {
        return Some((Plan::Release(r.builds.get(&t)?), r.version.clone()));
    }
    let (pkg, version, lock) = (a.npm.as_deref()?, a.version.as_deref()?, lockfile(cmd)?);
    let node = node_pin();
    let pin = format!("{version} (node {})", node.version);
    Some((
        Plan::Npm {
            pkg,
            version,
            lock,
            node: node.builds.get(&t)?,
        },
        pin,
    ))
}

fn own_dir(kind: AgentKind) -> Option<PathBuf> {
    Some(DIR.get()?.join(kind.as_str()))
}

fn node_dir() -> Option<PathBuf> {
    Some(DIR.get()?.join("node").join(&node_pin().version))
}

/// tab's own copy, whatever pin it came from.
fn own(kind: AgentKind) -> Option<Launch> {
    let (cmd, a) = spec(kind)?;
    let dir = own_dir(kind)?;
    if a.release.is_some() {
        let exe = dir.join(format!("{cmd}{}", std::env::consts::EXE_SUFFIX));
        return exe.is_file().then(|| Launch::program(exe));
    }
    let pkg = dir.join("node_modules").join(a.npm.as_deref()?);
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(pkg.join("package.json")).ok()?).ok()?;
    let bin = &manifest["bin"];
    let script = pkg.join(bin[cmd].as_str().or(bin.as_str())?);
    let node_root = node_dir()?;
    let node = node_exe(&node_root);
    (script.is_file() && node.is_file()).then(|| Launch {
        program: node,
        args: vec![script.into()],
        readable: vec![dir, node_root],
    })
}

/// Whether tab's own copy is from the current pin.
fn current(kind: AgentKind) -> bool {
    let (Some((_, pin)), Some(dir)) = (plan(kind), own_dir(kind)) else {
        return false;
    };
    std::fs::read_to_string(dir.join("version")).is_ok_and(|v| v.trim() == pin)
        && own(kind).is_some()
}

/// `kind`'s adapter as it is now: tab's own copy, then one on PATH. `None`
/// when it has neither, or no adapter program at all.
pub fn find(kind: AgentKind) -> Option<Launch> {
    let (cmd, _) = spec(kind)?;
    own(kind).or_else(|| resolve(cmd).ok().map(Launch::program))
}

/// Whether tab can install `kind`'s pinned adapter by itself here.
pub fn installable(kind: AgentKind) -> bool {
    plan(kind).is_some()
}

/// Whether `ensure` would install or update something.
pub fn needs_install(kind: AgentKind) -> bool {
    installable(kind) && !current(kind)
}

/// `kind`'s adapter, installing the pinned one first when tab has no
/// current copy. If that fails, an older copy or one on PATH still does.
/// Chats that start together wait for one install.
pub async fn ensure(
    kind: AgentKind,
    on_line: impl Fn(String) + Send + Sync + 'static,
) -> anyhow::Result<Launch> {
    if !needs_install(kind)
        && let Some(l) = find(kind)
    {
        return Ok(l);
    }
    static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _held = LOCK.lock().await;
    if !needs_install(kind)
        && let Some(l) = find(kind)
    {
        return Ok(l);
    }
    match install(kind, on_line).await {
        Ok(l) => Ok(l),
        Err(e) => find(kind).ok_or(e),
    }
}

/// Installs `kind`'s pinned adapter into tab's own folder, replacing any
/// copy there, passing progress to `on_line`.
pub async fn install(
    kind: AgentKind,
    on_line: impl Fn(String) + Send + Sync + 'static,
) -> anyhow::Result<Launch> {
    let name = catalog::harness(kind).name.as_str();
    let (cmd, _) =
        spec(kind).ok_or_else(|| anyhow::anyhow!("{name} has no ACP adapter to install."))?;
    let (plan, pin) = plan(kind).ok_or_else(|| {
        anyhow::anyhow!(
            "tab has no build of {cmd}, which {name} runs through, for this computer ({}).",
            target().unwrap_or_else(|| std::env::consts::OS.into())
        )
    })?;
    let dir = own_dir(kind).ok_or_else(|| anyhow::anyhow!("tab has no folder for tools yet."))?;
    let _ = tokio::fs::remove_dir_all(&dir).await;
    tokio::fs::create_dir_all(&dir).await?;
    match plan {
        Plan::Release(build) => {
            let staging = dir.join(".unpack");
            download_unpack(&format!("{cmd} {pin}"), build, &staging, &on_line).await?;
            // Just the adapter: claude-agent-acp's archive also carries a
            // Claude Code of its own, and tab points it at the person's.
            let exe = format!("{cmd}{}", std::env::consts::EXE_SUFFIX);
            let found = find_named(&staging, &exe, 2)
                .ok_or_else(|| anyhow::anyhow!("The download has no {exe} in it."));
            let r = match found {
                Ok(p) => tokio::fs::rename(&p, dir.join(&exe))
                    .await
                    .map_err(Into::into),
                Err(e) => Err(e),
            };
            let _ = tokio::fs::remove_dir_all(&staging).await;
            r?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = std::fs::Permissions::from_mode(0o755);
                tokio::fs::set_permissions(dir.join(&exe), mode).await?;
            }
        }
        Plan::Npm {
            pkg,
            version,
            lock,
            node,
        } => {
            let root = ensure_node(node, &on_line).await?;
            let manifest = serde_json::json!({
                "name": "tab-acp-adapter",
                "private": true,
                "dependencies": { pkg: version },
            });
            tokio::fs::write(
                dir.join("package.json"),
                serde_json::to_vec_pretty(&manifest)?,
            )
            .await?;
            tokio::fs::write(dir.join("package-lock.json"), lock).await?;
            // Exactly the lockfile's packages, each checked against its
            // hash, without install scripts. Optional ones are Codex's own
            // binaries, which tab doesn't use (it passes the person's).
            let mut c = command(&node_exe(&root));
            c.arg(npm_cli(&root))
                .args([
                    "ci",
                    "--ignore-scripts",
                    "--omit=optional",
                    "--no-audit",
                    "--no-fund",
                ])
                .current_dir(&dir);
            run_streaming(c, &format!("npm ci {pkg}@{version}"), on_line).await?;
        }
    }
    tokio::fs::write(dir.join("version"), &pin).await?;
    own(kind).ok_or_else(|| anyhow::anyhow!("Installed {cmd}, but its program isn't there."))
}

/// tab's pinned Node, downloaded the first time an npm adapter needs it.
async fn ensure_node(
    build: &Pinned,
    on_line: &(impl Fn(String) + Send + Sync),
) -> anyhow::Result<PathBuf> {
    let root = node_dir().ok_or_else(|| anyhow::anyhow!("tab has no folder for tools yet."))?;
    if node_exe(&root).is_file() && npm_cli(&root).is_file() {
        return Ok(root);
    }
    let staging = root.with_extension("unpack");
    download_unpack(
        &format!("Node {}", node_pin().version),
        build,
        &staging,
        on_line,
    )
    .await?;
    // The archive holds one folder, `node-v<version>-<target>`.
    let top = std::fs::read_dir(&staging)?
        .flatten()
        .map(|e| e.path())
        .find(|p| p.is_dir())
        .ok_or_else(|| anyhow::anyhow!("The Node download is empty."))?;
    let _ = tokio::fs::remove_dir_all(&root).await;
    tokio::fs::rename(&top, &root).await?;
    let _ = tokio::fs::remove_dir_all(&staging).await;
    anyhow::ensure!(
        node_exe(&root).is_file(),
        "The Node download has no node in it."
    );
    Ok(root)
}

fn node_exe(root: &Path) -> PathBuf {
    if cfg!(windows) {
        root.join("node.exe")
    } else {
        root.join("bin").join("node")
    }
}

fn npm_cli(root: &Path) -> PathBuf {
    let lib = if cfg!(windows) {
        root.to_path_buf()
    } else {
        root.join("lib")
    };
    lib.join("node_modules")
        .join("npm")
        .join("bin")
        .join("npm-cli.js")
}

fn http() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .user_agent(concat!("tab-desktop/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(30))
            .build()
            .expect("an HTTP client")
    })
}

/// Downloads `build`, refuses it unless it has the pinned sha256, and
/// unpacks it into `to` (emptied first).
async fn download_unpack(
    what: &str,
    build: &Pinned,
    to: &Path,
    on_line: &(impl Fn(String) + Send + Sync),
) -> anyhow::Result<()> {
    on_line(format!("Downloading {what}"));
    let mut resp = http()
        .get(&build.url)
        .timeout(Duration::from_secs(1800))
        .send()
        .await?
        .error_for_status()?;
    let total = resp.content_length();
    let _ = tokio::fs::remove_dir_all(to).await;
    tokio::fs::create_dir_all(to).await?;
    let name = build.url.rsplit('/').next().unwrap_or("download");
    let archive = to.join(name);
    let mut file = tokio::fs::File::create(&archive).await?;
    let (mut hash, mut got, mut shown) = (Sha256::new(), 0u64, 0u64);
    while let Some(chunk) = resp.chunk().await? {
        hash.update(&chunk);
        tokio::io::AsyncWriteExt::write_all(&mut file, &chunk).await?;
        got += chunk.len() as u64;
        if let Some(t) = total.filter(|t| *t > 0) {
            let pct = got * 100 / t;
            if pct >= shown + 10 {
                shown = pct - pct % 10;
                on_line(format!("Downloading {what}: {shown}%"));
            }
        }
    }
    tokio::io::AsyncWriteExt::flush(&mut file).await?;
    drop(file);
    let have: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if !have.eq_ignore_ascii_case(&build.sha256) {
        let _ = tokio::fs::remove_dir_all(to).await;
        anyhow::bail!("{name} didn't match the checksum tab has for it, so tab didn't keep it.");
    }
    on_line(format!("Unpacking {what}"));
    let r = unpack(&archive, to).await;
    let _ = tokio::fs::remove_file(&archive).await;
    r
}

/// Unpacks a `.zip` (in-process) or `.tar.gz` (the system's `tar`).
async fn unpack(archive: &Path, to: &Path) -> anyhow::Result<()> {
    if archive.extension().is_some_and(|e| e == "zip") {
        let (archive, to) = (archive.to_path_buf(), to.to_path_buf());
        return tokio::task::spawn_blocking(move || {
            zip::ZipArchive::new(std::fs::File::open(&archive)?)?.extract(&to)?;
            anyhow::Ok(())
        })
        .await?;
    }
    let out = command(Path::new("tar"))
        .arg("-xzf")
        .arg(archive)
        .arg("-C")
        .arg(to)
        .output()
        .await?;
    anyhow::ensure!(
        out.status.success(),
        "Unpacking the download failed: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(())
}

fn find_named(dir: &Path, name: &str, depth: u32) -> Option<PathBuf> {
    let entries: Vec<_> = std::fs::read_dir(dir).ok()?.flatten().collect();
    entries
        .iter()
        .map(|e| e.path())
        .find(|p| p.is_file() && p.file_name().is_some_and(|n| n == name))
        .or_else(|| {
            (depth > 0)
                .then(|| {
                    entries
                        .iter()
                        .map(|e| e.path())
                        .filter(|p| p.is_dir())
                        .find_map(|p| find_named(&p, name, depth - 1))
                })
                .flatten()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> PathBuf {
        let d = std::env::temp_dir().join(format!("tab-adapter-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn is_sha256(s: &str) -> bool {
        s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
    }

    #[test]
    fn every_adapter_is_pinned() {
        for h in catalog::harnesses() {
            let Some(acp) = h.acp.as_ref() else { continue };
            let Some(a) = acp.adapter.as_ref() else {
                continue;
            };
            let cmd = acp.command.as_deref().unwrap();
            if let Some(r) = &a.release {
                assert!(!r.builds.is_empty(), "{cmd}");
                for (t, b) in &r.builds {
                    assert!(is_sha256(&b.sha256), "{cmd} {t}");
                    assert!(
                        b.url
                            .starts_with(&format!("{}/releases/download/v{}/", a.repo, r.version)),
                        "{cmd} {t}: {}",
                        b.url
                    );
                }
                continue;
            }
            // npm: the lockfile pins exactly the version named, every
            // package has a hash, and none runs an install script.
            let (pkg, version) = (
                a.npm.as_deref().unwrap(),
                a.version.as_deref().expect("a pinned version"),
            );
            let lock: serde_json::Value =
                serde_json::from_str(lockfile(cmd).expect("a lockfile")).unwrap();
            assert_eq!(lock["packages"][""]["dependencies"][pkg], version, "{cmd}");
            assert_eq!(
                lock["packages"][format!("node_modules/{pkg}")]["version"],
                version,
                "{cmd}"
            );
            for (path, p) in lock["packages"].as_object().unwrap() {
                if path.is_empty() {
                    continue;
                }
                assert!(
                    p["integrity"]
                        .as_str()
                        .is_some_and(|i| i.starts_with("sha512-")),
                    "{path}"
                );
                assert!(
                    p.get("hasInstallScript").is_none(),
                    "{path} runs an install script"
                );
            }
        }
        for (t, b) in &node_pin().builds {
            assert!(is_sha256(&b.sha256), "node {t}");
            assert!(
                b.url
                    .starts_with(&format!("https://nodejs.org/dist/v{}/", node_pin().version)),
                "node {t}"
            );
        }
    }

    #[test]
    fn this_computer_has_a_build_of_each() {
        if target().is_some_and(|t| t.ends_with("musl")) {
            return;
        }
        for kind in [AgentKind::ClaudeCode, AgentKind::Codex] {
            assert!(plan(kind).is_some(), "{kind:?}");
        }
    }

    #[test]
    fn unpacks_a_zip() {
        use std::io::Write;
        let dir = scratch();
        let archive = dir.join("a.zip");
        let mut w = zip::ZipWriter::new(std::fs::File::create(&archive).unwrap());
        let opts = zip::write::SimpleFileOptions::default();
        for (n, body) in [("claude", "other"), ("pkg/claude-agent-acp", "adapter")] {
            w.start_file(n, opts).unwrap();
            w.write_all(body.as_bytes()).unwrap();
        }
        w.finish().unwrap();
        let to = dir.join("out");
        std::fs::create_dir(&to).unwrap();
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(unpack(&archive, &to))
            .unwrap();
        let exe = find_named(&to, "claude-agent-acp", 2).unwrap();
        assert_eq!(std::fs::read_to_string(exe).unwrap(), "adapter");
        assert_eq!(find_named(&to, "claude-agent-acp", 0), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Downloads the real pinned builds (a few hundred MB):
    /// `cargo test -p tab-core installs_pinned_adapters -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn installs_pinned_adapters() {
        let dir = scratch();
        set_dir(dir.clone());
        for kind in [AgentKind::ClaudeCode, AgentKind::Codex] {
            assert!(needs_install(kind));
            let l = ensure(kind, |l| eprintln!("{l}")).await.unwrap();
            assert!(l.program.is_file(), "{l:?}");
            assert!(current(kind) && !needs_install(kind), "{kind:?}");
            assert_eq!(find(kind), Some(l));
        }
        // Claude's: just the program and its pin.
        let left: Vec<_> = std::fs::read_dir(dir.join("claude_code"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(left.len(), 2, "{left:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
