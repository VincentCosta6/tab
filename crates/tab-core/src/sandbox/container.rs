//! Containers: the strongest isolation, the same on every OS.
//!
//! A workflow command step that names an image runs in Docker or Podman
//! with only its folder mounted, no capabilities, and no network unless the
//! step asks for it. This is also how Windows gets real isolation, and what
//! a self-hosted tab server would use to run agents and commands.

use std::path::Path;
use std::time::Duration;

use super::Network;

/// The container runtime that answers here: `pref` if set, else docker,
/// else podman.
pub async fn detect(pref: &str) -> Option<String> {
    let candidates: Vec<&str> = if pref.trim().is_empty() {
        vec!["docker", "podman"]
    } else {
        vec![pref.trim()]
    };
    for c in candidates {
        let Ok(exe) = which::which(c) else { continue };
        let mut cmd = tokio::process::Command::new(exe);
        cmd.args(["version", "--format", "{{.Server.Version}}"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000);
        if let Ok(Ok(s)) = tokio::time::timeout(Duration::from_secs(5), cmd.status()).await
            && s.success()
        {
            return Some(c.to_string());
        }
    }
    None
}

/// What to run in a container.
pub struct Spec<'a> {
    pub runtime: &'a str,
    /// Container name, so it can be removed if the step is stopped.
    pub name: &'a str,
    pub image: &'a str,
    pub program: &'a str,
    pub args: &'a [String],
    /// Mounted read-write at `/work`, the working directory.
    pub folder: &'a Path,
    pub network: &'a Network,
    pub env: &'a [(String, String)],
}

/// The `docker run` arguments for a spec.
pub fn run_args(s: &Spec) -> Vec<String> {
    let mut a: Vec<String> = [
        "run",
        "--rm",
        "-i",
        "--init",
        "--cap-drop",
        "ALL",
        "--security-opt",
        "no-new-privileges",
        "--pids-limit",
        "1024",
    ]
    .map(String::from)
    .to_vec();
    a.push("--name".into());
    a.push(s.name.into());
    a.push("--network".into());
    a.push(
        match s.network {
            Network::None => "none",
            _ => "bridge",
        }
        .into(),
    );
    // Files the step writes belong to the person, not root.
    #[cfg(unix)]
    {
        // SAFETY: plain getters.
        let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
        a.push("--user".into());
        a.push(format!("{uid}:{gid}"));
    }
    a.push("-v".into());
    a.push(format!("{}:/work", s.folder.display()));
    a.push("-w".into());
    a.push("/work".into());
    for (k, v) in s.env {
        a.push("-e".into());
        a.push(format!("{k}={v}"));
    }
    a.push(s.image.into());
    a.push(s.program.into());
    a.extend(s.args.iter().cloned());
    a
}

pub fn command(s: &Spec) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(s.runtime);
    cmd.args(run_args(s)).kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    cmd
}

/// Removes a container that's still running (the step was stopped or timed out).
pub async fn remove(runtime: &str, name: &str) {
    let _ = tokio::process::Command::new(runtime)
        .args(["rm", "-f", name])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await;
}
