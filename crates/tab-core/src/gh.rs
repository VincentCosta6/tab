//! The GitHub CLI (`gh`): find it, read its login, sign in through it, install it.
//!
//! tab borrows `gh`'s OAuth token instead of asking for a personal access
//! token. gh's default scopes (`repo`, `read:org`, `gist`) cover the
//! notifications API, so nothing extra is requested.

use std::path::PathBuf;
use std::process::Stdio;

use serde::Serialize;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const HOST: &str = "github.com";

#[derive(Debug, Clone, Serialize, Default)]
pub struct GhStatus {
    pub installed: bool,
    pub path: Option<String>,
    pub version: Option<String>,
    pub logged_in: bool,
    pub user: Option<String>,
    /// How to install gh on this platform, when tab can do it for you.
    pub install_command: Option<String>,
    pub error: Option<String>,
}

/// Progress from [`login`]: the one-time code to type at github.com/login/device.
#[derive(Debug, Clone, Serialize)]
pub struct GhLoginCode {
    pub code: String,
    pub url: String,
}

/// Finds `gh` on PATH, or where its installers put it. A GUI app on macOS
/// doesn't inherit the shell's PATH, and on Windows a fresh winget install
/// isn't on this process's PATH yet, so the usual locations are checked too.
pub fn find() -> Option<PathBuf> {
    if let Ok(p) = which::which("gh") {
        return Some(p);
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if cfg!(windows) {
        for var in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
            if let Ok(base) = std::env::var(var) {
                candidates.push(PathBuf::from(&base).join("GitHub CLI").join("gh.exe"));
                candidates.push(
                    PathBuf::from(&base)
                        .join("Programs")
                        .join("GitHub CLI")
                        .join("gh.exe"),
                );
            }
        }
    } else {
        candidates.extend(
            [
                "/opt/homebrew/bin/gh",
                "/usr/local/bin/gh",
                "/usr/bin/gh",
                "/home/linuxbrew/.linuxbrew/bin/gh",
            ]
            .map(PathBuf::from),
        );
    }
    candidates.into_iter().find(|p| p.is_file())
}

fn std_command(exe: &std::path::Path) -> std::process::Command {
    #[allow(unused_mut)]
    let mut cmd = std::process::Command::new(exe);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    cmd.env("GH_NO_UPDATE_NOTIFIER", "1").env("NO_COLOR", "1");
    cmd
}

fn command(exe: &std::path::Path) -> tokio::process::Command {
    let mut cmd = crate::agents::command(exe);
    cmd.env("GH_NO_UPDATE_NOTIFIER", "1").env("NO_COLOR", "1");
    cmd
}

/// gh's token for github.com, if gh is installed and signed in. Blocking; fast.
pub fn token() -> Option<String> {
    let exe = find()?;
    let out = std_command(&exe)
        .args(["auth", "token", "--hostname", HOST])
        .stdin(Stdio::null())
        .output()
        .ok()?;
    let t = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !t.is_empty()).then_some(t)
}

/// The command tab runs to install gh here, if there is one it can run unattended.
pub fn install_command() -> Option<(PathBuf, Vec<&'static str>)> {
    if cfg!(target_os = "macos") {
        let brew = which::which("brew").ok().or_else(|| {
            ["/opt/homebrew/bin/brew", "/usr/local/bin/brew"]
                .map(PathBuf::from)
                .into_iter()
                .find(|p| p.is_file())
        })?;
        Some((brew, vec!["install", "gh"]))
    } else if cfg!(windows) {
        let winget = which::which("winget").ok()?;
        Some((
            winget,
            vec![
                "install",
                "--id",
                "GitHub.cli",
                "-e",
                "--silent",
                "--accept-source-agreements",
                "--accept-package-agreements",
            ],
        ))
    } else {
        None
    }
}

fn describe(exe: &std::path::Path, args: &[&str]) -> String {
    let name = exe
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    format!("{name} {}", args.join(" "))
}

pub async fn status() -> GhStatus {
    let mut s = GhStatus {
        install_command: install_command().map(|(exe, args)| describe(&exe, &args)),
        ..Default::default()
    };
    let Some(exe) = find() else {
        return s;
    };
    s.installed = true;
    s.path = Some(exe.display().to_string());
    if let Ok(out) = command(&exe).arg("--version").output().await {
        s.version = String::from_utf8_lossy(&out.stdout)
            .lines()
            .next()
            .map(|l| {
                l.trim_start_matches("gh version ")
                    .split_whitespace()
                    .next()
                    .unwrap_or(l)
                    .to_string()
            });
    }
    let has_token = tokio::task::spawn_blocking(token)
        .await
        .ok()
        .flatten()
        .is_some();
    if !has_token {
        return s;
    }
    s.logged_in = true;
    match command(&exe)
        .args(["api", "user", "--jq", ".login"])
        .stdin(Stdio::null())
        .output()
        .await
    {
        Ok(out) if out.status.success() => {
            s.user = Some(String::from_utf8_lossy(&out.stdout).trim().to_string());
        }
        Ok(out) => s.error = Some(String::from_utf8_lossy(&out.stderr).trim().to_string()),
        Err(e) => s.error = Some(e.to_string()),
    }
    s
}

/// Pulls the device code out of gh's prompt: "! First copy your one-time code: ABCD-1234".
fn parse_code(line: &str) -> Option<String> {
    let (_, rest) = line.split_once("one-time code:")?;
    let code = rest.split_whitespace().next()?;
    (code.len() >= 8 && code.contains('-')).then(|| code.to_string())
}

/// Signs in through gh's browser flow. `on_code` gets the one-time code as soon
/// as gh prints it; this returns once the user finishes (or gh gives up).
pub async fn login(on_code: impl Fn(GhLoginCode) + Send) -> anyhow::Result<GhStatus> {
    let exe = find().ok_or_else(|| anyhow::anyhow!("The GitHub CLI isn't installed."))?;
    let mut child = command(&exe)
        .args([
            "auth",
            "login",
            "--hostname",
            HOST,
            "--git-protocol",
            "https",
            "--web",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    // gh asks for Enter before opening the browser.
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(b"\n").await;
    }
    let mut out = BufReader::new(child.stdout.take().expect("piped")).lines();
    let mut err = BufReader::new(child.stderr.take().expect("piped")).lines();
    let mut tail = Vec::new();
    let (mut out_done, mut err_done) = (false, false);
    while !(out_done && err_done) {
        let line = tokio::select! {
            l = out.next_line(), if !out_done => l?.or_else(|| { out_done = true; None }),
            l = err.next_line(), if !err_done => l?.or_else(|| { err_done = true; None }),
        };
        let Some(line) = line else { continue };
        if let Some(code) = parse_code(&line) {
            on_code(GhLoginCode {
                code,
                url: format!("https://{HOST}/login/device"),
            });
        }
        tail.push(line);
    }
    let done = tokio::time::timeout(std::time::Duration::from_secs(900), child.wait()).await;
    let ok = matches!(done, Ok(Ok(s)) if s.success());
    let s = status().await;
    if !ok && !s.logged_in {
        let msg = tail
            .iter()
            .rev()
            .find(|l| !l.trim().is_empty())
            .cloned()
            .unwrap_or_else(|| "gh auth login didn't finish.".into());
        anyhow::bail!(msg);
    }
    Ok(s)
}

/// Installs gh with the platform's package manager (Homebrew or winget).
pub async fn install() -> anyhow::Result<GhStatus> {
    let (exe, args) = install_command().ok_or_else(|| {
        anyhow::anyhow!("tab can't install gh here. Get it from https://cli.github.com")
    })?;
    let out = command(&exe)
        .args(&args)
        .stdin(Stdio::null())
        .output()
        .await?;
    let s = status().await;
    if !s.installed {
        let text = String::from_utf8_lossy(if out.stderr.is_empty() {
            &out.stdout
        } else {
            &out.stderr
        })
        .trim()
        .lines()
        .last()
        .unwrap_or("")
        .to_string();
        anyhow::bail!(
            "Installing gh didn't work{}",
            if text.is_empty() {
                ".".into()
            } else {
                format!(": {text}")
            }
        );
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_device_code() {
        assert_eq!(
            parse_code("! First copy your one-time code: AB12-CD34"),
            Some("AB12-CD34".into())
        );
        assert_eq!(
            parse_code("Press Enter to open github.com in your browser..."),
            None
        );
    }
}
