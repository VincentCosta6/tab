//! Command steps: a program run in tab's sandbox.
//!
//! The program gets its folder to change (the run's own scratch folder
//! unless the step names one), build caches, and the network hosts the
//! step lists, and nothing of tab's environment that looks secret. With an
//! image it runs in a Docker or Podman container instead, which is the same
//! on every OS and the only isolation on Windows.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::AsyncReadExt;

use crate::sandbox::{self, Network, container};

/// Output kept per stream.
const MAX_OUTPUT: usize = 32 * 1024;

pub struct Spec {
    pub program: String,
    pub args: Vec<String>,
    pub folder: PathBuf,
    pub network: Network,
    pub env: Vec<(String, String)>,
    pub image: Option<String>,
    pub timeout: Duration,
}

/// What a finished command produced. A non-zero exit is still `Ok`; the
/// runtime marks the step failed.
pub struct Done {
    pub exit_code: Option<i32>,
    pub output: Value,
}

fn tail(bytes: &[u8]) -> String {
    let s = String::from_utf8_lossy(bytes);
    if s.len() <= MAX_OUTPUT {
        return s.into_owned();
    }
    let mut start = s.len() - MAX_OUTPUT;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    format!("…{}", &s[start..])
}

async fn collect(
    mut child: impl std::ops::DerefMut<Target = tokio::process::Child>,
    timeout: Duration,
) -> anyhow::Result<(Option<i32>, Vec<u8>, Vec<u8>, bool)> {
    let mut out = child.stdout.take().expect("piped");
    let mut err = child.stderr.take().expect("piped");
    let read_out = tokio::spawn(async move {
        let mut b = Vec::new();
        let _ = out.read_to_end(&mut b).await;
        b
    });
    let read_err = tokio::spawn(async move {
        let mut b = Vec::new();
        let _ = err.read_to_end(&mut b).await;
        b
    });
    let (code, timed_out) = match tokio::time::timeout(timeout, child.wait()).await {
        Ok(status) => (status?.code(), false),
        Err(_) => (None, true),
    };
    // Dropping `child` (a sandbox guard) ends its whole tree.
    drop(child);
    let o = tokio::time::timeout(Duration::from_secs(5), read_out)
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default();
    let e = tokio::time::timeout(Duration::from_secs(5), read_err)
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default();
    Ok((code, o, e, timed_out))
}

pub async fn run(spec: Spec) -> anyhow::Result<Done> {
    std::fs::create_dir_all(&spec.folder)?;
    let sb = sandbox::current();
    let (code, out, err, timed_out, how) = if let Some(image) = &spec.image {
        let runtime = container::detect(&sb.config().container_runtime)
            .await
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "This step runs in the {image} container, which needs Docker or Podman running."
                )
            })?;
        let name = format!("tab-step-{}", uuid::Uuid::new_v4().simple());
        let mut cmd = container::command(&container::Spec {
            runtime: &runtime,
            name: &name,
            image,
            program: &spec.program,
            args: &spec.args,
            folder: &sandbox::real(&spec.folder),
            network: &spec.network,
            env: &spec.env,
        });
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = cmd
            .spawn()
            .map_err(|e| anyhow::anyhow!("starting {runtime} failed: {e}"))?;
        let r = collect(Box::new(child), spec.timeout).await;
        if r.as_ref().map(|r| r.3).unwrap_or(true) {
            container::remove(&runtime, &name).await;
        }
        let (code, o, e, t) = r?;
        (code, o, e, t, runtime)
    } else {
        let exe = resolve(&spec.program, &spec.folder)?;
        let policy = sb.isolated_policy(std::slice::from_ref(&spec.folder), spec.network.clone());
        let args: Vec<std::ffi::OsString> = spec.args.iter().map(Into::into).collect();
        let mut prepared = sb.command(&exe, &args, &spec.folder, &policy).await?;
        for (k, v) in &spec.env {
            prepared.cmd.env(k, v);
        }
        prepared
            .cmd
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let how = if prepared.enforced {
            format!("{:?}", sb.backend()).to_lowercase()
        } else {
            "none".to_string()
        };
        let child = prepared
            .spawn()
            .map_err(|e| anyhow::anyhow!("starting {} failed: {e}", spec.program))?;
        let (code, o, e, t) = collect(child, spec.timeout).await?;
        (code, o, e, t, how)
    };
    if timed_out {
        anyhow::bail!(
            "took longer than {}s and was stopped",
            spec.timeout.as_secs()
        );
    }
    Ok(Done {
        exit_code: code,
        output: json!({
            "exit_code": code,
            "stdout": tail(&out),
            "stderr": tail(&err),
            "sandbox": how,
        }),
    })
}

/// A program by name on PATH, or a path (relative ones from the folder).
fn resolve(program: &str, folder: &Path) -> anyhow::Result<PathBuf> {
    let p = Path::new(program);
    if p.components().count() > 1 {
        let full = if p.is_absolute() {
            p.to_path_buf()
        } else {
            folder.join(p)
        };
        anyhow::ensure!(full.exists(), "{} doesn't exist", full.display());
        return Ok(full);
    }
    crate::lsp::find(program)
        .ok_or_else(|| anyhow::anyhow!("`{program}` isn't installed or isn't on PATH"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn folder() -> PathBuf {
        let d = std::env::temp_dir().join(format!("tab-cmd-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn runs_a_program_and_keeps_its_output() {
        let f = folder();
        let d = run(Spec {
            program: "sh".into(),
            args: vec![
                "-c".into(),
                "echo out; echo err >&2; echo hi > made; exit 3".into(),
            ],
            folder: f.clone(),
            network: Network::None,
            env: vec![("GREETING".into(), "hello".into())],
            image: None,
            timeout: Duration::from_secs(20),
        })
        .await
        .unwrap();
        assert_eq!(d.exit_code, Some(3));
        assert_eq!(d.output["stdout"], "out\n");
        assert_eq!(d.output["stderr"], "err\n");
        assert!(f.join("made").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stops_at_the_timeout() {
        let e = run(Spec {
            program: "sleep".into(),
            args: vec!["30".into()],
            folder: folder(),
            network: Network::None,
            env: vec![],
            image: None,
            timeout: Duration::from_millis(300),
        })
        .await
        .err()
        .unwrap();
        assert!(e.to_string().contains("longer than"), "{e}");
    }

    #[test]
    fn long_output_keeps_the_end() {
        let s = "a".repeat(MAX_OUTPUT) + "THE END";
        let t = tail(s.as_bytes());
        assert!(t.ends_with("THE END"));
        assert!(t.len() <= MAX_OUTPUT + 4);
    }
}
