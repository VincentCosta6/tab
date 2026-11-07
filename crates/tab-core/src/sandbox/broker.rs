//! Brokered command-line tools: `gh` and git's network commands run by tab,
//! with the GitHub connection's token, instead of inside the agent's sandbox.
//!
//! Agents are trained on `gh pr create` and `git push`, so they keep typing
//! those. What changes is where the command runs:
//!
//! 1. A chat's PATH starts with a folder of shims named `gh` and `git`. Each
//!    starts tab's own binary as `tab __tab_cli <tool> <args>`.
//! 2. That client sorts the command ([`classify`]). Local git commands
//!    (`status`, `commit`, `rebase`) run right there, as before. Anything
//!    that talks to GitHub is sent to tab's local server (`/cli`).
//! 3. tab asks the person first when the command is destructive (a force
//!    push, `gh pr merge`, `gh repo delete`), or when a chat about someone
//!    else's code wants to change anything. Then it runs the real tool in
//!    the chat's own sandbox, with the token in that process alone, and
//!    writes the call to the audit log next to the connector's API calls.
//!
//! The agent never holds the token: `GH_TOKEN` is dropped from its
//! environment, gh's config folder is hidden, and on macOS and Linux the
//! real `gh` can't be started from inside the sandbox.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use serde::{Deserialize, Serialize};

use tab_domain::{CliAuth, CliChange, CliEffect, CliInfo, CliMatch};

use super::Policy;

/// A CLI some connection signs in, and which connection.
#[derive(Debug, Clone)]
pub struct Brokered {
    pub connector: String,
    pub cli: CliInfo,
}

impl Brokered {
    pub fn name(&self) -> &str {
        &self.cli.name
    }

    /// The real program, found the way tab finds it, outside any chat.
    pub fn real(&self) -> Option<PathBuf> {
        match self.cli.name.as_str() {
            "gh" => crate::gh::find(),
            n => which::which(n).ok(),
        }
    }

    pub fn classify(&self, args: &[String]) -> CliMatch {
        self.cli.classify(args)
    }
}

/// Every CLI a connection type declares.
pub fn all() -> Vec<Brokered> {
    crate::connectors::registry()
        .iter()
        .map(|f| f.descriptor())
        .flat_map(|d| {
            let id = d.id.clone();
            d.clis.into_iter().map(move |cli| Brokered {
                connector: id.clone(),
                cli,
            })
        })
        .collect()
}

pub fn find(name: &str) -> Option<Brokered> {
    all().into_iter().find(|b| b.cli.name == name)
}

/// One line for the approval card and the audit log.
pub fn describe(tool: &str, args: &[String]) -> String {
    let mut s = String::from(tool);
    for a in args {
        s.push(' ');
        if a.is_empty() || a.contains(char::is_whitespace) || a.contains('\'') {
            s.push_str(&format!("'{}'", a.replace('\'', "'\\''")));
        } else {
            s.push_str(a);
        }
    }
    if s.chars().count() > 300 {
        s = s.chars().take(300).collect::<String>() + "…";
    }
    s
}

/// What the client sends to `/cli`.
#[derive(Debug, Serialize, Deserialize)]
pub struct Request {
    pub tool: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    /// Base64, when a flag reads from `-`.
    #[serde(default)]
    pub stdin: Option<String>,
}

/// What `/cli` sends back.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Reply {
    pub code: i32,
    /// Base64.
    pub stdout: String,
    /// Base64.
    pub stderr: String,
}

impl Reply {
    pub fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&B64.decode(&self.stdout).unwrap_or_default()).into_owned()
    }

    pub fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&B64.decode(&self.stderr).unwrap_or_default()).into_owned()
    }

    pub fn refused(why: &str) -> Self {
        Self {
            code: 1,
            stdout: String::new(),
            stderr: B64.encode(format!("tab: {why}\n")),
        }
    }
}

/// Where a chat's shims reach tab.
pub struct Endpoint<'a> {
    /// tab's `/cli` URL.
    pub url: &'a str,
    pub token: &'a str,
    pub session: &'a str,
}

/// A chat's shims: put `bin` first on PATH, set `git_env`, and point
/// `CLAUDE_ENV_FILE` at `env_file` so shells that rebuild PATH from the
/// person's startup files still find them.
pub struct Shims {
    pub bin: PathBuf,
    pub env_file: PathBuf,
    /// `GIT_CONFIG_*` for the chat's git (see [`git_config`]).
    pub git_env: Vec<(String, String)>,
}

impl Shims {
    /// PATH with the shims first.
    pub fn path_var(&self) -> OsString {
        let mut parts = vec![self.bin.clone()];
        if let Some(p) = std::env::var_os("PATH") {
            parts.extend(std::env::split_paths(&p));
        }
        std::env::join_paths(parts).unwrap_or_default()
    }

    pub fn apply(&self, cmd: &mut tokio::process::Command) {
        cmd.env("PATH", self.path_var());
        // Claude Code rebuilds PATH from the person's shell setup and then
        // sources this, so the shims stay first.
        cmd.env("CLAUDE_ENV_FILE", &self.env_file);
        for (k, v) in &self.git_env {
            cmd.env(k, v);
        }
    }
}

/// The chat's git settings: remotes on hosts a connection signs in go to
/// tab's endpoint (`git_base`, which names the chat), which knows the chat
/// by tab's token; and credentials for every other host come from the
/// person's own helpers through tab, which never hands out a connection's.
pub fn git_config(git_base: &str, token: &str, helper: &Path) -> Vec<(String, String)> {
    let mut cfg = Vec::new();
    for b in all() {
        if let CliAuth::GitEndpoint { remotes, .. } = &b.cli.auth {
            for r in remotes {
                cfg.push((
                    format!("url.{git_base}{}/.insteadOf", b.connector),
                    r.clone(),
                ));
            }
        }
    }
    cfg.push((
        format!("http.{git_base}.extraHeader"),
        format!("Authorization: Bearer {token}"),
    ));
    // An empty value drops the helpers from the person's config.
    cfg.push(("credential.helper".into(), String::new()));
    cfg.push((
        "credential.helper".into(),
        helper.display().to_string().replace('\\', "/"),
    ));
    let mut env = vec![("GIT_CONFIG_COUNT".to_string(), cfg.len().to_string())];
    for (i, (k, v)) in cfg.into_iter().enumerate() {
        env.push((format!("GIT_CONFIG_KEY_{i}"), k));
        env.push((format!("GIT_CONFIG_VALUE_{i}"), v));
    }
    env
}

/// Writes the chat's shims into `dir`: one per CLI tab runs (`gh`), and
/// `git-credential-tab`.
pub fn write_shims(dir: &Path, ep: &Endpoint, tab_exe: &Path) -> std::io::Result<Option<Shims>> {
    let mut tools: Vec<(String, String)> = all()
        .into_iter()
        .filter(|b| matches!(b.cli.auth, CliAuth::Env { .. }))
        .filter_map(|b| b.real().map(|p| (b.cli.name, p.display().to_string())))
        .collect();
    let git = which::which("git").ok();
    if git.is_some() {
        tools.push(("git-credential-tab".into(), String::new()));
    }
    if tools.is_empty() {
        return Ok(None);
    }
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin)?;
    for (tool, real) in &tools {
        let sub = if tool == "git-credential-tab" {
            "git-credential"
        } else {
            tool
        };
        let vars = [
            ("TAB_CLI_URL", ep.url.to_string()),
            ("TAB_CLI_TOKEN", ep.token.to_string()),
            ("TAB_CLI_SESSION", ep.session.to_string()),
            ("TAB_CLI_REAL", real.clone()),
        ];
        // A POSIX script on every OS: Claude Code's shell on Windows is Git Bash.
        let mut sh = String::from("#!/bin/sh\n");
        for (k, v) in &vars {
            sh.push_str(&format!("{k}={}; export {k}\n", sh_quote(v)));
        }
        sh.push_str(&format!(
            "exec {} __tab_cli {sub} \"$@\"\n",
            sh_quote(&posix_path(tab_exe)),
        ));
        let f = bin.join(tool);
        std::fs::write(&f, sh)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755))?;
        }
        #[cfg(windows)]
        {
            let mut cmd = String::from("@echo off\r\n");
            for (k, v) in &vars {
                cmd.push_str(&format!("set \"{k}={v}\"\r\n"));
            }
            cmd.push_str(&format!(
                "\"{}\" __tab_cli {sub} %*\r\nexit /b %ERRORLEVEL%\r\n",
                tab_exe.display(),
            ));
            std::fs::write(bin.join(format!("{tool}.cmd")), cmd)?;
        }
    }
    let git_env = match git {
        Some(_) => {
            let base = ep.url.trim_end_matches("/cli").to_string() + "/git/" + ep.session + "/";
            let helper = bin.join(if cfg!(windows) {
                "git-credential-tab.cmd"
            } else {
                "git-credential-tab"
            });
            git_config(&base, ep.token, &helper)
        }
        None => Vec::new(),
    };
    let mut env = format!(
        "# Written by tab: CLIs your connections sign in go through tab.\nPATH={}:\"$PATH\"; export PATH\n",
        sh_quote(&posix_path(&bin))
    );
    for (k, v) in &git_env {
        env.push_str(&format!("{k}={}; export {k}\n", sh_quote(v)));
    }
    let env_file = dir.join("shell-env.sh");
    std::fs::write(&env_file, env)?;
    Ok(Some(Shims {
        bin,
        env_file,
        git_env,
    }))
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// `C:\Users\me` as Git Bash spells it, `/c/Users/me`; other paths as they are.
fn posix_path(p: &Path) -> String {
    let s = p.display().to_string();
    let b = s.as_bytes();
    if b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic() {
        return format!(
            "/{}{}",
            (b[0] as char).to_ascii_lowercase(),
            s[2..].replace('\\', "/")
        );
    }
    s
}

/// `tab __tab_cli <tool> <args>`: what the shims run. Returns the exit code.
pub fn client_main(argv: Vec<OsString>) -> i32 {
    match client(argv) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("tab: {e}");
            1
        }
    }
}

fn client(argv: Vec<OsString>) -> anyhow::Result<i32> {
    let mut it = argv.into_iter();
    let tool = it
        .next()
        .and_then(|t| t.into_string().ok())
        .ok_or_else(|| anyhow::anyhow!("usage: tab __tab_cli <tool> <args>"))?;
    if tool == "git-credential" {
        return credential_client(it.map(|a| a.to_string_lossy().into_owned()).collect());
    }
    let tool = find(&tool).ok_or_else(|| anyhow::anyhow!("tab doesn't run {tool}"))?;
    let raw: Vec<OsString> = it.collect();
    let args: Vec<String> = raw
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let verdict = tool.classify(&args);
    if verdict.effect == CliEffect::Local {
        let real = var("TAB_CLI_REAL").ok_or_else(|| anyhow::anyhow!("TAB_CLI_REAL isn't set"))?;
        let mut cmd = std::process::Command::new(real);
        cmd.args(&raw);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            return Err(cmd.exec().into());
        }
        #[cfg(not(unix))]
        return Ok(cmd.status()?.code().unwrap_or(1));
    }
    if verdict.effect == CliEffect::Blocked {
        eprintln!(
            "tab: {}",
            verdict
                .reason
                .as_deref()
                .unwrap_or("this command isn't available in chats")
        );
        return Ok(1);
    }
    let (url, token, session) = (
        var("TAB_CLI_URL").ok_or_else(|| anyhow::anyhow!("TAB_CLI_URL isn't set"))?,
        var("TAB_CLI_TOKEN").unwrap_or_default(),
        var("TAB_CLI_SESSION").unwrap_or_default(),
    );
    let reads_stdin = args
        .iter()
        .any(|a| a == "-" || a.ends_with("@-") || a.ends_with("=-"));
    let stdin = if reads_stdin {
        let mut b = Vec::new();
        std::io::Read::read_to_end(&mut std::io::stdin(), &mut b)?;
        Some(B64.encode(b))
    } else {
        None
    };
    let req = Request {
        tool: tool.cli.name,
        args,
        cwd: std::env::current_dir()?,
        stdin,
    };
    let reply = send(&url, &token, &session, &req)?;
    use std::io::Write;
    std::io::stdout().write_all(&B64.decode(reply.stdout).unwrap_or_default())?;
    std::io::stderr().write_all(&B64.decode(reply.stderr).unwrap_or_default())?;
    Ok(reply.code)
}

/// git's credential helper protocol (`get`, `store`, `erase` with
/// `key=value` lines on stdin), answered by tab.
fn credential_client(args: Vec<String>) -> anyhow::Result<i32> {
    let var = |k: &str| std::env::var(k).unwrap_or_default();
    let mut b = Vec::new();
    std::io::Read::read_to_end(&mut std::io::stdin(), &mut b)?;
    let req = Request {
        tool: "git-credential".into(),
        args,
        cwd: std::env::current_dir().unwrap_or_default(),
        stdin: Some(B64.encode(b)),
    };
    let reply = send(
        &var("TAB_CLI_URL"),
        &var("TAB_CLI_TOKEN"),
        &var("TAB_CLI_SESSION"),
        &req,
    )?;
    use std::io::Write;
    std::io::stdout().write_all(&B64.decode(reply.stdout).unwrap_or_default())?;
    std::io::stderr().write_all(&B64.decode(reply.stderr).unwrap_or_default())?;
    Ok(reply.code)
}

/// Answers a chat's git credential request. Hosts a connection signs in
/// get nothing: their traffic goes through tab's endpoint, which adds the
/// token itself. Other hosts get what the person's own helpers give, as
/// they would outside tab.
pub async fn credential(op: &str, input: &[u8]) -> Reply {
    let text = String::from_utf8_lossy(input);
    let host = text
        .lines()
        .find_map(|l| l.strip_prefix("host="))
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let host = host.split(':').next().unwrap_or("").to_string();
    let owned = all().iter().any(|b| match &b.cli.auth {
        CliAuth::GitEndpoint { url, remotes } => std::iter::once(url)
            .chain(remotes)
            .any(|r| host_of(r).is_some_and(|h| h == host)),
        _ => false,
    });
    if owned || host.is_empty() {
        return Reply::default();
    }
    let sub = match op {
        "get" => "fill",
        "store" => "approve",
        "erase" => "reject",
        _ => return Reply::default(),
    };
    let Ok(git) = which::which("git") else {
        return Reply::default();
    };
    let mut cmd = crate::agents::command(&git);
    cmd.args(["credential", sub])
        .current_dir(std::env::temp_dir())
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let Ok(mut child) = cmd.spawn() else {
        return Reply::default();
    };
    if let Some(mut w) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        let _ = w.write_all(input).await;
    }
    match tokio::time::timeout(Duration::from_secs(30), child.wait_with_output()).await {
        Ok(Ok(o)) if o.status.success() => Reply {
            code: 0,
            stdout: B64.encode(o.stdout),
            stderr: String::new(),
        },
        _ => Reply::default(),
    }
}

/// `github.com` from `https://github.com/`, `git@github.com:` or `ssh://git@github.com/`.
fn host_of(remote: &str) -> Option<String> {
    let rest = remote.split_once("://").map(|(_, r)| r).unwrap_or(remote);
    let rest = rest.rsplit_once('@').map(|(_, r)| r).unwrap_or(rest);
    let host = rest.split(['/', ':']).next()?;
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

/// Sends a command to tab's `/cli` and waits for it. Blocking.
pub fn send(url: &str, token: &str, session: &str, req: &Request) -> anyhow::Result<Reply> {
    let body = post(url, token, session, &serde_json::to_vec(req)?)?;
    Ok(serde_json::from_slice(&body)?)
}

/// A plain HTTP/1.1 POST to tab's local server.
fn post(url: &str, token: &str, session: &str, body: &[u8]) -> anyhow::Result<Vec<u8>> {
    use std::io::{Read, Write};
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| anyhow::anyhow!("unexpected URL {url}"))?;
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let mut s = std::net::TcpStream::connect(host)
        .map_err(|e| anyhow::anyhow!("tab isn't reachable ({e}); is the app running?"))?;
    let head = format!(
        "POST /{path} HTTP/1.1\r\nHost: {host}\r\nAuthorization: Bearer {token}\r\nX-Tab-Session: {session}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    s.write_all(head.as_bytes())?;
    s.write_all(body)?;
    let mut resp = Vec::new();
    s.read_to_end(&mut resp)?;
    let end = resp
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| anyhow::anyhow!("tab sent a broken reply"))?;
    let head = String::from_utf8_lossy(&resp[..end]).to_ascii_lowercase();
    let mut body = resp[end + 4..].to_vec();
    if head.contains("transfer-encoding: chunked") {
        body = dechunk(&body);
    }
    let status = head.split_whitespace().nth(1).unwrap_or("");
    anyhow::ensure!(
        status == "200",
        "tab refused the command ({status}): {}",
        String::from_utf8_lossy(&body).trim()
    );
    Ok(body)
}

fn dechunk(mut b: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(nl) = b.windows(2).position(|w| w == b"\r\n") {
        let size = std::str::from_utf8(&b[..nl])
            .ok()
            .and_then(|s| usize::from_str_radix(s.split(';').next().unwrap_or("").trim(), 16).ok())
            .unwrap_or(0);
        if size == 0 || b.len() < nl + 2 + size {
            break;
        }
        out.extend_from_slice(&b[nl + 2..nl + 2 + size]);
        b = &b[(nl + 4 + size).min(b.len())..];
    }
    out
}

/// The sandbox a brokered command runs in: the chat's own, so it can't
/// write anywhere the agent couldn't, minus anything blocked for the agent,
/// and with no inherited GitHub token (tab passes its own).
pub fn policy_for(chat: &Policy) -> Policy {
    let mut p = chat.clone();
    p.block_exec.clear();
    p.network = super::Network::Any;
    p.drop_env
        .extend(super::env::GITHUB_KEYS.iter().map(|k| k.to_string()));
    p
}

/// git settings the repository itself may not override while the token is
/// there: an agent can write `.git/config` and hooks, and git runs programs
/// named in them. Returns `(key, value)` pairs that win over the repo's.
pub async fn git_hardening(git: &Path, cwd: &Path) -> Vec<(String, String)> {
    let mut out = vec![
        ("protocol.ext.allow".to_string(), "never".to_string()),
        (
            "core.hooksPath".to_string(),
            empty_hooks().display().to_string(),
        ),
    ];
    let listed = crate::agents::command(git)
        .args([
            "config",
            "--list",
            "--show-scope",
            "--includes",
            "--name-only",
        ])
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .await;
    if let Ok(o) = listed {
        for line in String::from_utf8_lossy(&o.stdout).lines() {
            let Some((scope, key)) = line.split_once('\t') else {
                continue;
            };
            if matches!(scope, "local" | "worktree") && runs_a_program(key) {
                out.push((key.to_string(), String::new()));
            }
        }
    }
    out
}

/// Whether a git setting names a program git may run.
pub fn runs_a_program(key: &str) -> bool {
    let last = key.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    [
        "command",
        "cmd",
        "program",
        "helper",
        "process",
        "clean",
        "smudge",
        "textconv",
        "driver",
        "pager",
        "editor",
        "askpass",
        "uploadpack",
        "receivepack",
        "proxy",
        "fsmonitor",
        "hookspath",
        "sshcommand",
    ]
    .iter()
    .any(|s| last.ends_with(s))
}

fn empty_hooks() -> PathBuf {
    let d = std::env::temp_dir().join("tab-no-hooks");
    let _ = std::fs::create_dir_all(&d);
    d
}

/// Runs a brokered command in `policy` with `token`. Never fails: problems
/// come back as the command's stderr.
pub async fn execute(
    tool: &Brokered,
    req: &Request,
    policy: &Policy,
    token: Option<&str>,
) -> Reply {
    match run(tool, req, policy, token).await {
        Ok(r) => r,
        Err(e) => Reply::refused(&e.to_string()),
    }
}

async fn run(
    tool: &Brokered,
    req: &Request,
    policy: &Policy,
    token: Option<&str>,
) -> anyhow::Result<Reply> {
    use std::process::Stdio;
    use tokio::io::AsyncWriteExt;
    let exe = tool
        .real()
        .ok_or_else(|| anyhow::anyhow!("{} isn't installed", tool.name()))?;
    anyhow::ensure!(req.cwd.is_absolute(), "the working folder must be absolute");
    let args: Vec<OsString> = req.args.iter().map(OsString::from).collect();
    let mut prepared = super::current()
        .command(&exe, &args, &req.cwd, &policy_for(policy))
        .await?;
    let cmd = &mut prepared.cmd;
    cmd.stdin(if req.stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .env("GH_PROMPT_DISABLED", "1")
    .env("GH_NO_UPDATE_NOTIFIER", "1")
    .env("GIT_TERMINAL_PROMPT", "0")
    .env("GCM_INTERACTIVE", "never")
    .env("GIT_EDITOR", "true")
    .env("GIT_MERGE_AUTOEDIT", "no");
    let CliAuth::Env { var } = &tool.cli.auth else {
        anyhow::bail!("{} runs in the chat, not through tab", tool.name());
    };
    if let Some(t) = token {
        cmd.env(var, t);
    }
    if tool.name() == "gh" {
        // gh's own config (and its stored sign-in) stays out of it.
        let d = std::env::temp_dir().join("tab-gh-config");
        let _ = std::fs::create_dir_all(&d);
        cmd.env("GH_CONFIG_DIR", d);
    }
    // `gh pr checkout` and friends run git in the checkout with the token in
    // their environment: the repository's own config can't name programs.
    if let Ok(git) = which::which("git") {
        let cfg = git_hardening(&git, &req.cwd).await;
        cmd.env("GIT_CONFIG_COUNT", cfg.len().to_string());
        for (i, (k, v)) in cfg.iter().enumerate() {
            cmd.env(format!("GIT_CONFIG_KEY_{i}"), k);
            cmd.env(format!("GIT_CONFIG_VALUE_{i}"), v);
        }
    }
    let mut child = prepared.spawn()?;
    if let (Some(input), Some(mut w)) = (&req.stdin, child.stdin.take()) {
        let bytes = B64.decode(input).unwrap_or_default();
        tokio::spawn(async move {
            let _ = w.write_all(&bytes).await;
        });
    }
    let (mut out, mut err) = (child.stdout.take(), child.stderr.take());
    let read = async {
        use tokio::io::AsyncReadExt;
        let (mut o, mut e) = (Vec::new(), Vec::new());
        let (a, b) = tokio::join!(
            async {
                if let Some(s) = out.as_mut() {
                    let _ = s.read_to_end(&mut o).await;
                }
            },
            async {
                if let Some(s) = err.as_mut() {
                    let _ = s.read_to_end(&mut e).await;
                }
            }
        );
        let _ = (a, b);
        (o, e)
    };
    let ((o, e), status) = tokio::time::timeout(Duration::from_secs(15 * 60), async {
        let r = read.await;
        (r, child.wait().await)
    })
    .await
    .map_err(|_| anyhow::anyhow!("stopped after 15 minutes"))?;
    Ok(Reply {
        code: status?.code().unwrap_or(1),
        stdout: B64.encode(o),
        stderr: B64.encode(e),
    })
}

/// One ref a push changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefUpdate {
    pub old: String,
    pub new: String,
    pub name: String,
}

impl RefUpdate {
    pub fn creates(&self) -> bool {
        self.old.bytes().all(|b| b == b'0')
    }

    pub fn deletes(&self) -> bool {
        self.new.bytes().all(|b| b == b'0')
    }

    /// `main` for `refs/heads/main`.
    pub fn short(&self) -> &str {
        self.name
            .strip_prefix("refs/heads/")
            .or_else(|| self.name.strip_prefix("refs/tags/"))
            .unwrap_or(&self.name)
    }
}

/// The ref updates at the start of a `git-receive-pack` request body, its
/// capabilities, and where they end. None until the whole list (up to its
/// flush packet) has arrived.
pub fn parse_push(buf: &[u8]) -> Option<(Vec<RefUpdate>, String, usize)> {
    let mut pos = 0;
    let mut updates = Vec::new();
    let mut caps = String::new();
    loop {
        let head = std::str::from_utf8(buf.get(pos..pos + 4)?).ok()?;
        let len = usize::from_str_radix(head, 16).ok()?;
        if len == 0 {
            return Some((updates, caps, pos + 4));
        }
        if len < 4 {
            return None;
        }
        let line = buf.get(pos + 4..pos + len)?;
        pos += len;
        let line = String::from_utf8_lossy(line);
        let line = line.trim_end_matches('\n');
        let line = match line.split_once('\0') {
            Some((l, c)) => {
                caps = c.to_string();
                l
            }
            None => line,
        };
        let mut parts = line.split(' ');
        if let (Some(old), Some(new), Some(name)) = (parts.next(), parts.next(), parts.next())
            && old.len() >= 40
            && new.len() >= 40
        {
            updates.push(RefUpdate {
                old: old.into(),
                new: new.into(),
                name: name.into(),
            });
        }
    }
}

#[cfg(test)]
pub(crate) fn tests_pkt(data: &[u8]) -> Vec<u8> {
    pkt(data)
}

fn pkt(data: &[u8]) -> Vec<u8> {
    let mut v = format!("{:04x}", data.len() + 4).into_bytes();
    v.extend_from_slice(data);
    v
}

/// A `git-receive-pack` answer that turns every update down with `why`, so
/// git prints a clear `! [remote rejected] main (why)` instead of an HTTP
/// error.
pub fn push_refused(updates: &[RefUpdate], caps: &str, why: &str) -> Vec<u8> {
    let mut report = pkt(b"unpack ok\n");
    for u in updates {
        report.extend(pkt(format!("ng {} {why}\n", u.name).as_bytes()));
    }
    report.extend_from_slice(b"0000");
    let caps: Vec<&str> = caps.split(' ').collect();
    let band = if caps.contains(&"side-band-64k") {
        65515
    } else if caps.contains(&"side-band") {
        995
    } else {
        return report;
    };
    let mut out = Vec::new();
    for chunk in report.chunks(band) {
        let mut d = vec![1u8];
        d.extend_from_slice(chunk);
        out.extend(pkt(&d));
    }
    let mut msg = vec![2u8];
    msg.extend_from_slice(format!("tab: {why}\n").as_bytes());
    out.extend(pkt(&msg));
    out.extend_from_slice(b"0000");
    out
}

/// What a finished change touched, for the app to refresh. `repo` is the
/// `owner/name` the command named or the checkout's GitHub remote.
pub fn change(tool: &Brokered, m: &CliMatch, args: &[String], repo: Option<String>) -> CliChange {
    let resource = m.resource.clone().unwrap_or_default();
    // A URL names both.
    let from_url = args.iter().find_map(|a| parse_github_url(a));
    let repo = from_url
        .as_ref()
        .map(|(r, _)| r.clone())
        .or_else(|| {
            let mut it = args.iter();
            while let Some(a) = it.next() {
                if a == "-R" || a == "--repo" {
                    return it.next().cloned();
                }
                if let Some(v) = a.strip_prefix("--repo=") {
                    return Some(v.to_string());
                }
            }
            None
        })
        .or(repo);
    let number = from_url.and_then(|(_, n)| n).or_else(|| {
        tool.cli
            .positionals(args)
            .iter()
            .skip(2)
            .find_map(|a| a.trim_start_matches('#').parse::<u64>().ok())
    });
    let url = match (&repo, number, resource.as_str()) {
        (Some(r), Some(n), "pull") => Some(format!("https://github.com/{r}/pull/{n}")),
        (Some(r), Some(n), "issue") => Some(format!("https://github.com/{r}/issues/{n}")),
        _ => None,
    };
    CliChange {
        connector: tool.connector.clone(),
        command: describe(tool.name(), args),
        effect: m.effect,
        resource,
        repo,
        number,
        url,
    }
}

/// `https://github.com/o/r/pull/12` → (`o/r`, 12).
fn parse_github_url(s: &str) -> Option<(String, Option<u64>)> {
    let rest = s
        .strip_prefix("https://github.com/")
        .or_else(|| s.strip_prefix("git@github.com:"))?;
    let mut parts = rest.trim_end_matches(".git").split('/');
    let (o, r) = (parts.next()?, parts.next()?);
    if o.is_empty() || r.is_empty() {
        return None;
    }
    let n = match (parts.next(), parts.next()) {
        (Some("pull" | "issues"), Some(n)) => n.parse().ok(),
        _ => None,
    };
    Some((format!("{o}/{}", r.trim_end_matches(".git")), n))
}

/// The checkout's GitHub repository, from its `origin` remote.
pub async fn repo_of(cwd: &Path) -> Option<String> {
    let git = which::which("git").ok()?;
    let out = crate::agents::command(&git)
        .args(["remote", "get-url", "origin"])
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .await
        .ok()?;
    parse_github_url(String::from_utf8_lossy(&out.stdout).trim()).map(|(r, _)| r)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(tool: &str, s: &str) -> CliEffect {
        let args: Vec<String> = s.split_whitespace().map(String::from).collect();
        find(tool).unwrap().classify(&args).effect
    }

    #[test]
    fn sorts_gh_commands() {
        use CliEffect::*;
        assert_eq!(v("gh", ""), Read);
        assert_eq!(v("gh", "pr list"), Read);
        assert_eq!(v("gh", "pr view 12 --json title"), Read);
        assert_eq!(v("gh", "-R o/r pr checks 12"), Read);
        assert_eq!(v("gh", "pr create --title x --body y"), Write);
        assert_eq!(v("gh", "pr comment 3 -b hi"), Write);
        assert_eq!(v("gh", "pr review 3 --approve"), Write);
        assert_eq!(v("gh", "issue close 4"), Write);
        assert_eq!(v("gh", "pr merge 12 --squash"), Destructive);
        assert_eq!(v("gh", "repo delete o/r --yes"), Destructive);
        assert_eq!(v("gh", "release delete v1"), Destructive);
        assert_eq!(v("gh", "issue transfer 1 o/x"), Destructive);
        assert_eq!(v("gh", "secret set X"), Destructive);
        assert_eq!(v("gh", "secret list"), Read);
        assert_eq!(v("gh", "repo edit --visibility public"), Destructive);
        assert_eq!(v("gh", "repo edit --description x"), Write);
        assert_eq!(v("gh", "search prs foo"), Read);
        assert_eq!(v("gh", "auth token"), Blocked);
        assert_eq!(v("gh", "auth status"), Read);
        assert_eq!(v("gh", "extension install x/y"), Blocked);
        assert_eq!(v("gh", "config set editor vim"), Blocked);
        assert_eq!(v("gh", "config get editor"), Read);
    }

    #[test]
    fn sorts_gh_api_by_method() {
        use CliEffect::*;
        assert_eq!(v("gh", "api repos/o/r/pulls"), Read);
        assert_eq!(v("gh", "api repos/o/r/issues -f title=x"), Write);
        assert_eq!(v("gh", "api -X PATCH repos/o/r"), Write);
        assert_eq!(v("gh", "api --method=DELETE repos/o/r"), Destructive);
        assert_eq!(v("gh", "api -XGET search/issues -f q=x"), Read);
        assert_eq!(v("gh", "api graphql -f query={viewer{login}}"), Read);
        assert_eq!(v("gh", "api graphql -f query=mutation{addStar}"), Write);
    }

    #[test]
    fn git_runs_in_the_chat() {
        assert_eq!(v("git", "push --force origin main"), CliEffect::Local);
        assert_eq!(v("git", "status"), CliEffect::Local);
    }

    #[test]
    fn git_reaches_github_only_through_tab() {
        let env = git_config(
            "http://127.0.0.1:9/git/s1/",
            "tok",
            Path::new("/x/git-credential-tab"),
        );
        let get = |k: &str| {
            env.iter()
                .find(|(n, _)| n == k)
                .map(|(_, v)| v.clone())
                .unwrap()
        };
        let count: usize = get("GIT_CONFIG_COUNT").parse().unwrap();
        let pairs: Vec<(String, String)> = (0..count)
            .map(|i| {
                (
                    get(&format!("GIT_CONFIG_KEY_{i}")),
                    get(&format!("GIT_CONFIG_VALUE_{i}")),
                )
            })
            .collect();
        assert!(pairs.contains(&(
            "url.http://127.0.0.1:9/git/s1/github/.insteadOf".into(),
            "git@github.com:".into()
        )));
        assert!(pairs.contains(&(
            "http.http://127.0.0.1:9/git/s1/.extraHeader".into(),
            "Authorization: Bearer tok".into()
        )));
        let helpers: Vec<&str> = pairs
            .iter()
            .filter(|(k, _)| k == "credential.helper")
            .map(|(_, v)| v.as_str())
            .collect();
        assert_eq!(helpers, ["", "/x/git-credential-tab"]);
        assert_eq!(host_of("git@github.com:"), Some("github.com".into()));
        assert_eq!(host_of("ssh://git@github.com/"), Some("github.com".into()));
    }

    #[test]
    fn github_declares_its_clis() {
        let names: Vec<String> = all().into_iter().map(|b| b.cli.name).collect();
        assert_eq!(names, ["gh", "git"]);
        assert!(all().iter().all(|b| b.connector == "github"));
    }

    #[test]
    fn says_what_a_change_touched() {
        let gh = find("gh").unwrap();
        let args: Vec<String> = "pr merge 12 --squash"
            .split(' ')
            .map(String::from)
            .collect();
        let c = change(&gh, &gh.classify(&args), &args, Some("o/r".into()));
        assert_eq!(c.resource, "pull");
        assert_eq!(c.number, Some(12));
        assert_eq!(c.url.as_deref(), Some("https://github.com/o/r/pull/12"));

        let args: Vec<String> = ["pr", "review", "https://github.com/a/b/pull/7", "--approve"]
            .map(String::from)
            .to_vec();
        let c = change(&gh, &gh.classify(&args), &args, Some("o/r".into()));
        assert_eq!((c.repo.as_deref(), c.number), (Some("a/b"), Some(7)));

        let args: Vec<String> = "-R x/y issue comment 3 -b hi"
            .split(' ')
            .map(String::from)
            .collect();
        let c = change(&gh, &gh.classify(&args), &args, None);
        assert_eq!(c.url.as_deref(), Some("https://github.com/x/y/issues/3"));
    }

    #[test]
    fn describes_commands_for_people() {
        let args: Vec<String> = ["pr", "create", "--title", "Fix it's bug"]
            .map(String::from)
            .to_vec();
        assert_eq!(
            describe("gh", &args),
            "gh pr create --title 'Fix it'\\''s bug'"
        );
    }

    #[test]
    fn repo_config_cannot_name_programs() {
        for k in [
            "core.fsmonitor",
            "core.sshCommand",
            "core.hooksPath",
            "filter.lfs.smudge",
            "filter.x.process",
            "diff.x.textconv",
            "merge.x.driver",
            "credential.helper",
            "credential.https://x.helper",
            "remote.origin.uploadpack",
            "core.pager",
            "gpg.program",
            "http.proxy",
        ] {
            assert!(runs_a_program(k), "{k}");
        }
        for k in [
            "user.name",
            "remote.origin.url",
            "branch.main.merge",
            "core.autocrlf",
        ] {
            assert!(!runs_a_program(k), "{k}");
        }
    }

    #[test]
    fn reads_pushes_and_refuses_them_the_way_git_expects() {
        let z = "0".repeat(40);
        let (a, b) = ("a".repeat(40), "b".repeat(40));
        let mut body =
            pkt(format!("{a} {b} refs/heads/main\0report-status side-band-64k\n").as_bytes());
        body.extend(pkt(format!("{a} {z} refs/heads/old\n").as_bytes()));
        body.extend(pkt(format!("{z} {b} refs/heads/new\n").as_bytes()));
        assert!(parse_push(&body).is_none(), "no flush yet");
        body.extend_from_slice(b"0000PACK...");
        let (u, caps, end) = parse_push(&body).unwrap();
        assert_eq!(&body[end..], b"PACK...");
        assert_eq!(caps, "report-status side-band-64k");
        assert_eq!(u.len(), 3);
        assert!(!u[0].creates() && !u[0].deletes());
        assert!(u[1].deletes() && u[2].creates());
        assert_eq!(u[0].short(), "main");

        let r = push_refused(&u[..1], &caps, "not allowed");
        let s = String::from_utf8_lossy(&r);
        assert!(
            s.contains("unpack ok") && s.contains("ng refs/heads/main not allowed"),
            "{s}"
        );
        assert!(s.contains("tab: not allowed") && s.ends_with("0000"), "{s}");
        // Without side-band, the bare report.
        let r = push_refused(&u[..1], "report-status", "no");
        assert!(r.starts_with(b"000eunpack ok\n"));
    }

    #[test]
    fn decodes_chunked_bodies() {
        assert_eq!(dechunk(b"3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n"), b"abcde");
    }

    #[test]
    fn spells_windows_paths_for_git_bash() {
        assert_eq!(posix_path(Path::new(r"C:\Users\me\tab")), "/c/Users/me/tab");
        assert_eq!(posix_path(Path::new("/usr/bin")), "/usr/bin");
    }
}
