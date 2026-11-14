//! Harnesses that run one process per turn and print JSON lines (Codex's
//! `exec --json`, Pi's `--mode json`). Each adapter builds the command line
//! and reads the events; this runs the process under the chat's sandbox
//! policy and handles stopping it.
//!
//! These CLIs take no more input once started, so a message sent mid-turn
//! waits and runs as the next turn.

use std::process::Stdio;
use std::sync::Arc;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio_util::sync::CancellationToken;

use super::{AgentHub, Args, Steer};
use crate::model::Session;

/// Reads one harness's events.
pub(super) trait Events {
    fn handle(&mut self, v: &Value);
    /// The turn's end arrived (successful or not).
    fn finished(&self) -> bool;
    /// Strict mode: something the person set up (not tab's) showed up, such
    /// as a call to another MCP server. The turn stops.
    fn leaked(&self) -> Option<String> {
        None
    }
}

pub(super) struct Turn<'a> {
    /// For error messages: `Codex`, `Pi`.
    pub name: &'static str,
    pub exe: &'a std::path::Path,
    pub args: Args,
    /// Extra environment, set after the sandbox scrubs tab's own.
    pub env: Vec<(String, String)>,
    /// Written to stdin, which then closes.
    pub stdin: Option<String>,
}

pub(super) async fn run(
    hub: &Arc<AgentHub>,
    session: &Session,
    turn: Turn<'_>,
    events: &mut impl Events,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    // Nothing reads steering: what's sent from here on waits for the next turn.
    hub.steering_close(&session.id);
    let mut queued = hub.steering_open(&session.id);
    let result = run_process(hub, session, turn, events, cancel).await;
    let mut later = Vec::new();
    while let Ok(s) = queued.try_recv() {
        if let Steer::Message(m) = s {
            later.push(m);
        }
    }
    if !later.is_empty() {
        hub.followups
            .lock()
            .unwrap()
            .entry(session.id.clone())
            .or_default()
            .extend(later);
    }
    result
}

async fn run_process(
    hub: &Arc<AgentHub>,
    session: &Session,
    turn: Turn<'_>,
    events: &mut impl Events,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    let policy = hub.policy(session);
    let mut prepared = crate::sandbox::current()
        .command(
            turn.exe,
            &turn.args.0,
            std::path::Path::new(&session.cwd),
            &policy,
        )
        .await?;
    hub.add_shims(session, &mut prepared.cmd);
    for (k, v) in &turn.env {
        prepared.cmd.env(k, v);
    }
    prepared
        .cmd
        .stdin(if turn.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    tracing::debug!(session = %session.id, harness = turn.name, resume = ?session.agent_session_id, sandboxed = prepared.enforced, "starting turn");
    let mut child = prepared.spawn().map_err(|e| {
        anyhow::anyhow!(
            "starting {} ({}) failed: {e}",
            turn.name,
            turn.exe.display()
        )
    })?;
    if let Some(input) = turn.stdin {
        let mut stdin = child.stdin.take().expect("piped");
        stdin.write_all(input.as_bytes()).await?;
        drop(stdin);
    }
    let stdout = child.stdout.take().expect("piped");
    let mut stderr = child.stderr.take().expect("piped");
    let err_task = tokio::spawn(async move {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s).await;
        s
    });
    let mut lines = BufReader::new(stdout).lines();
    loop {
        tokio::select! {
            line = lines.next_line() => match line? {
                Some(l) if !l.trim().is_empty() => match serde_json::from_str::<Value>(l.trim()) {
                    Ok(v) => {
                        events.handle(&v);
                        if let Some(what) = events.leaked() {
                            child.kill_tree();
                            anyhow::bail!(
                                "Strict mode stopped this turn: {} used {what}, which isn't one of tab's connections.",
                                turn.name
                            );
                        }
                    }
                    Err(_) => tracing::debug!(line = %l, harness = turn.name, "non-json output"),
                },
                Some(_) => {}
                None => break,
            },
            _ = cancel.cancelled() => {
                child.kill_tree();
                return Ok(());
            }
        }
    }
    let status = child.wait().await?;
    let stderr = err_task.await.unwrap_or_default();
    if !events.finished() {
        anyhow::bail!(
            "{} exited ({status}) before finishing.{}",
            turn.name,
            tail(&stderr)
        );
    }
    Ok(())
}

/// The last few lines of stderr, for an error message.
pub(super) fn tail(stderr: &str) -> String {
    let lines: Vec<&str> = stderr.lines().filter(|l| !l.trim().is_empty()).collect();
    let last = &lines[lines.len().saturating_sub(8)..];
    if last.is_empty() {
        String::new()
    } else {
        format!("\n{}", last.join("\n"))
    }
}

/// Runs a harness once with no sandbox wrapper (it gets no tools), for the
/// background helpers. Returns stdout.
pub(super) async fn capture(
    exe: &std::path::Path,
    args: &[std::ffi::OsString],
    stdin: Option<&str>,
    timeout: std::time::Duration,
) -> anyhow::Result<String> {
    let mut cmd = super::command(exe);
    cmd.args(args)
        .current_dir(std::env::temp_dir())
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    if let Some(input) = stdin {
        let mut s = child.stdin.take().expect("piped");
        s.write_all(input.as_bytes()).await?;
    }
    let out = tokio::time::timeout(timeout, child.wait_with_output()).await??;
    anyhow::ensure!(
        out.status.success(),
        "it exited ({}){}",
        out.status,
        tail(&String::from_utf8_lossy(&out.stderr))
    );
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}
