//! ACP adapter programs (claude-agent-acp, codex-acp) that tab keeps itself,
//! so a harness's chats work without the person installing anything. An
//! adapter with release builds (`acp.adapter.release`) is downloaded from
//! its GitHub release and checked against the digest GitHub publishes, so
//! it needs no Node; the rest go in with npm. Either way into
//! `<tools>/acp/<harness id>`, never globally. One already on PATH is used
//! when tab has no copy of its own.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use sha2::{Digest, Sha256};

use super::catalog::{self, AcpAdapter};
use super::{AgentKind, command, resolve, run_streaming};

static DIR: OnceLock<PathBuf> = OnceLock::new();

/// Where tab keeps adapters: `<dir>/<harness id>`.
pub fn set_dir(dir: PathBuf) {
    let _ = DIR.set(dir);
}

fn own_dir(kind: AgentKind) -> Option<PathBuf> {
    Some(DIR.get()?.join(kind.as_str()))
}

/// The adapter's program and what installs it, when `kind` runs through one.
fn spec(kind: AgentKind) -> Option<(&'static str, &'static AcpAdapter)> {
    let acp = catalog::harness(kind).acp.as_ref()?;
    Some((acp.command.as_deref()?, acp.adapter.as_ref()?))
}

/// tab's own copy, if it has one.
fn own_exe(kind: AgentKind) -> Option<PathBuf> {
    let (cmd, _) = spec(kind)?;
    let dir = own_dir(kind)?;
    let exts: &[&str] = if cfg!(windows) {
        &[".exe", ".cmd", ""]
    } else {
        &[""]
    };
    [dir.clone(), dir.join("node_modules").join(".bin")]
        .iter()
        .flat_map(|d| exts.iter().map(move |e| d.join(format!("{cmd}{e}"))))
        .find(|p| p.is_file())
}

/// `kind`'s adapter: tab's own copy, then one on PATH. `None` when it has
/// none, or no adapter program at all.
pub fn find(kind: AgentKind) -> Option<PathBuf> {
    let (cmd, _) = spec(kind)?;
    own_exe(kind).or_else(|| resolve(cmd).ok())
}

/// Whether tab can put `kind`'s adapter in by itself on this computer.
pub fn installable(kind: AgentKind) -> bool {
    spec(kind).is_some_and(|(_, a)| {
        a.release.is_some() && release_asset(a).is_some()
            || a.npm.is_some() && which::which("npm").is_ok()
    })
}

/// `kind`'s adapter, installed first when it's missing. Chats that start
/// together wait for one install.
pub async fn ensure(
    kind: AgentKind,
    on_line: impl Fn(String) + Send + Sync + 'static,
) -> anyhow::Result<PathBuf> {
    if let Some(p) = find(kind) {
        return Ok(p);
    }
    static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _held = LOCK.lock().await;
    if let Some(p) = find(kind) {
        return Ok(p);
    }
    install(kind, on_line).await
}

/// Installs `kind`'s adapter into tab's own folder, replacing any copy
/// there, passing progress to `on_line`.
pub async fn install(
    kind: AgentKind,
    on_line: impl Fn(String) + Send + Sync + 'static,
) -> anyhow::Result<PathBuf> {
    let name = catalog::harness(kind).name.as_str();
    let (cmd, adapter) =
        spec(kind).ok_or_else(|| anyhow::anyhow!("{name} has no ACP adapter to install."))?;
    let dir = own_dir(kind).ok_or_else(|| anyhow::anyhow!("tab has no folder for tools yet."))?;
    tokio::fs::create_dir_all(&dir).await?;
    if let Some(asset) = release_asset(adapter) {
        download(cmd, adapter, &asset, &dir, &on_line).await?;
    } else if let (Some(pkg), Ok(npm)) = (adapter.npm.as_deref(), which::which("npm")) {
        let mut c = command(&npm);
        c.arg("install")
            .arg("--prefix")
            .arg(&dir)
            .args(["--no-audit", "--no-fund", pkg]);
        run_streaming(c, &format!("npm install {pkg}"), on_line).await?;
    } else {
        anyhow::bail!(
            "{name} runs through {cmd}, and setting it up on this computer needs Node.js (https://nodejs.org)."
        );
    }
    own_exe(kind).ok_or_else(|| anyhow::anyhow!("Installed {cmd}, but its program isn't there."))
}

/// This computer's release build of `adapter`, by file name.
fn release_asset(adapter: &AcpAdapter) -> Option<String> {
    let template = adapter.release.as_deref()?;
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
    let target = format!("{os}-{arch}{}", if musl { "-musl" } else { "" });
    let ext = if os == "linux" { "tar.gz" } else { "zip" };
    Some(format!("{}.{ext}", template.replace("{target}", &target)))
}

/// `owner/name` from the adapter's repository URL.
fn repo_slug(adapter: &AcpAdapter) -> Option<&str> {
    let s = adapter.repo.strip_prefix("https://github.com/")?;
    Some(s.trim_end_matches('/').trim_end_matches(".git"))
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

#[derive(serde::Deserialize)]
struct Release {
    tag_name: String,
    assets: Vec<Asset>,
}

#[derive(serde::Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
    /// `sha256:<hex>`, as GitHub computed it on upload.
    digest: Option<String>,
}

/// Downloads the newest release's `asset`, checks its digest and keeps just
/// the adapter's program from it (the archive also carries a Claude Code of
/// its own; tab points the adapter at the person's).
async fn download(
    cmd: &str,
    adapter: &AcpAdapter,
    asset: &str,
    dir: &Path,
    on_line: &(impl Fn(String) + Send + Sync),
) -> anyhow::Result<()> {
    let slug = repo_slug(adapter)
        .ok_or_else(|| anyhow::anyhow!("{} isn't a GitHub repository.", adapter.repo))?;
    let release: Release = http()
        .get(format!(
            "https://api.github.com/repos/{slug}/releases/latest"
        ))
        .header("Accept", "application/vnd.github+json")
        .timeout(Duration::from_secs(30))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let a = release
        .assets
        .iter()
        .find(|a| a.name == asset)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "{cmd} {} has no build for this computer ({asset}).",
                release.tag_name
            )
        })?;
    let want = a
        .digest
        .as_deref()
        .and_then(|d| d.strip_prefix("sha256:"))
        .ok_or_else(|| anyhow::anyhow!("GitHub lists no checksum for {asset}."))?
        .to_ascii_lowercase();
    on_line(format!("Downloading {cmd} {}", release.tag_name));
    let mut resp = http()
        .get(&a.browser_download_url)
        .timeout(Duration::from_secs(1800))
        .send()
        .await?
        .error_for_status()?;
    let total = resp.content_length();
    let archive = dir.join(format!(".download-{asset}"));
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
                on_line(format!("Downloading {cmd} {}: {shown}%", release.tag_name));
            }
        }
    }
    tokio::io::AsyncWriteExt::flush(&mut file).await?;
    drop(file);
    let have: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if have != want {
        let _ = tokio::fs::remove_file(&archive).await;
        anyhow::bail!("{asset} didn't match its checksum, so tab didn't keep it.");
    }
    on_line(format!("Unpacking {cmd}"));
    let exe_name = format!("{cmd}{}", std::env::consts::EXE_SUFFIX);
    let staged = dir.join(format!(".new-{exe_name}"));
    let r = if asset.ends_with(".zip") {
        let (archive, staged, exe_name) = (archive.clone(), staged.clone(), exe_name.clone());
        tokio::task::spawn_blocking(move || unzip_one(&archive, &exe_name, &staged)).await?
    } else {
        untar_one(&archive, &exe_name, dir, &staged).await
    };
    let _ = tokio::fs::remove_file(&archive).await;
    r?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755)).await?;
    }
    tokio::fs::rename(&staged, dir.join(&exe_name)).await?;
    let _ = tokio::fs::write(dir.join("version"), &release.tag_name).await;
    Ok(())
}

/// Copies the zip's `name` entry (at any depth) to `to`.
fn unzip_one(archive: &Path, name: &str, to: &Path) -> anyhow::Result<()> {
    let mut zip = zip::ZipArchive::new(std::fs::File::open(archive)?)?;
    let path = zip
        .file_names()
        .filter_map(Result::ok)
        .find(|n| n.rsplit('/').next() == Some(name))
        .map(|n| n.into_owned())
        .ok_or_else(|| anyhow::anyhow!("The download has no {name} in it."))?;
    let mut entry = zip.by_name(&path)?;
    let mut out = std::fs::File::create(to)?;
    std::io::copy(&mut entry, &mut out)?;
    Ok(())
}

/// Unpacks a `.tar.gz` with the system's `tar` and moves its `name` to `to`.
async fn untar_one(archive: &Path, name: &str, dir: &Path, to: &Path) -> anyhow::Result<()> {
    let tmp = dir.join(".unpack");
    let _ = tokio::fs::remove_dir_all(&tmp).await;
    tokio::fs::create_dir_all(&tmp).await?;
    let out = command(Path::new("tar"))
        .arg("-xzf")
        .arg(archive)
        .arg("-C")
        .arg(&tmp)
        .output()
        .await?;
    let found = out
        .status
        .success()
        .then(|| find_named(&tmp, name, 2))
        .flatten();
    let r = match found {
        Some(p) => tokio::fs::rename(&p, to).await.map_err(Into::into),
        None => Err(anyhow::anyhow!(
            "Unpacking the download failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )),
    };
    let _ = tokio::fs::remove_dir_all(&tmp).await;
    r
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

    #[test]
    fn claude_code_has_a_release_build_for_this_computer() {
        let (cmd, a) = spec(AgentKind::ClaudeCode).expect("claude-agent-acp");
        assert_eq!(cmd, "claude-agent-acp");
        assert_eq!(repo_slug(a), Some("agentclientprotocol/claude-agent-acp"));
        let asset = release_asset(a).expect("a build for this OS and CPU");
        assert!(asset.starts_with("claude-agent-acp-"), "{asset}");
        let ext = if cfg!(target_os = "linux") {
            ".tar.gz"
        } else {
            ".zip"
        };
        assert!(asset.ends_with(ext), "{asset}");
    }

    fn scratch() -> PathBuf {
        let d = std::env::temp_dir().join(format!("tab-adapter-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn unzip_keeps_only_the_program() {
        use std::io::Write;
        let dir = scratch();
        let archive = dir.join("a.zip");
        let mut w = zip::ZipWriter::new(std::fs::File::create(&archive).unwrap());
        let opts = zip::write::SimpleFileOptions::default();
        for (n, body) in [("claude", "other"), ("claude-agent-acp", "adapter")] {
            w.start_file(n, opts).unwrap();
            w.write_all(body.as_bytes()).unwrap();
        }
        w.finish().unwrap();
        let to = dir.join("out");
        unzip_one(&archive, "claude-agent-acp", &to).unwrap();
        assert_eq!(std::fs::read_to_string(&to).unwrap(), "adapter");
        assert!(unzip_one(&archive, "missing", &to).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Downloads the real release (about 200 MB): `cargo test -p tab-core
    /// downloads_claude_agent_acp -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn downloads_claude_agent_acp() {
        let dir = scratch();
        set_dir(dir.clone());
        let exe = install(AgentKind::ClaudeCode, |l| eprintln!("{l}"))
            .await
            .unwrap();
        assert_eq!(
            exe,
            dir.join("claude_code")
                .join(format!("claude-agent-acp{}", std::env::consts::EXE_SUFFIX))
        );
        assert_eq!(find(AgentKind::ClaudeCode), Some(exe));
        let left: Vec<_> = std::fs::read_dir(dir.join("claude_code"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(left.len(), 2, "just the program and its version: {left:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn finds_a_file_one_folder_down() {
        let dir = scratch();
        std::fs::create_dir(dir.join("pkg")).unwrap();
        std::fs::write(dir.join("pkg").join("x"), "").unwrap();
        assert_eq!(find_named(&dir, "x", 2), Some(dir.join("pkg").join("x")));
        assert_eq!(find_named(&dir, "x", 0), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
