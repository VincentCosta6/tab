//! Claude Code adapter: one `claude -p` process per turn, resumed by session id,
//! read as `stream-json`.

use std::collections::HashSet;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio_util::sync::CancellationToken;

use super::{AgentHub, Steer};
use crate::model::{AgentKind, Session, SessionStatus};
use crate::sandbox::Trust;

/// Claude Code permission modes tab passes through. `bypassPermissions` is
/// deliberately not offered: approvals belong to the person.
const MODES: &[&str] = &["default", "acceptEdits", "plan", "auto", "dontAsk"];

/// Tool calls in flight, by id. Claude Code reads a steering message only
/// between tool calls, so "Send now" interrupts while one is running.
type OpenTools = Arc<Mutex<HashSet<String>>>;

/// The id of the `initialize` request tab sends at the start of each turn.
const INIT_REQUEST: &str = "tab-init";

/// Most of a message quoted back in a reminder.
const QUOTE: usize = 300;

/// Messages the person sent mid-turn, until the agent answers them in text.
///
/// Claude Code shows a queued message to the model with the next tool
/// result. The model then sometimes answers only in its thinking, which the
/// person never sees, and goes on to its next tool call: from their side
/// the message was ignored. So once a message has been shown, the agent's
/// next step must start with visible text. If it starts with a tool call
/// instead, tab says so in the chat and reminds the agent, once per message.
#[derive(Default)]
pub(super) struct Unanswered {
    /// Written to stdin; the agent hasn't been shown them yet.
    sent: Vec<String>,
    /// Shown to the agent, with no reply in text since.
    shown: Vec<String>,
}

impl Unanswered {
    /// The person's message went to the agent.
    pub(super) fn sent(&mut self, text: &str) {
        self.sent.push(text.to_string());
    }

    /// A tool result came back: queued messages go to the model with it.
    fn shown(&mut self) {
        self.shown.append(&mut self.sent);
    }

    /// The agent said something visible: that answers what it was shown.
    fn replied(&mut self) {
        self.shown.clear();
    }

    /// The agent made a tool call. Returns the messages it went past
    /// without a word, if any.
    fn skipped(&mut self) -> Vec<String> {
        std::mem::take(&mut self.shown)
    }
}

/// What tab tells the agent when it went past the person's messages.
fn reminder(skipped: &[String]) -> String {
    let quoted: Vec<String> = skipped
        .iter()
        .map(|m| {
            let m = m.trim();
            let cut: String = m.chars().take(QUOTE).collect();
            if cut.len() < m.len() {
                format!("\"{cut}…\"")
            } else {
                format!("\"{cut}\"")
            }
        })
        .collect();
    format!(
        "(From tab, not the person.) You went on without replying to the person's message {}. \
         Your thinking isn't shown to them, so to them it looks ignored. Reply to it now in visible \
         text, then carry on.",
        quoted.join(" and ")
    )
}

/// The stdin line for a steering command, if it needs one: a message is
/// queued as a user turn; an interrupt (Esc in the terminal) is sent only
/// while a tool runs, since otherwise the message is read at the next step.
fn steer_line(steer: Steer, tools_running: bool, n: u64) -> Option<Value> {
    match steer {
        Steer::Message(text) | Steer::Reminder(text) => {
            Some(json!({ "type": "user", "message": { "role": "user", "content": text } }))
        }
        Steer::Interrupt if tools_running => Some(json!({
            "type": "control_request",
            "request_id": format!("tab-interrupt-{n}"),
            "request": { "subtype": "interrupt" },
        })),
        Steer::Interrupt => None,
        Steer::Mode(mode) if MODES.contains(&mode.as_str()) => Some(json!({
            "type": "control_request",
            "request_id": format!("tab-mode-{n}"),
            "request": { "subtype": "set_permission_mode", "mode": mode },
        })),
        Steer::Mode(_) => None,
    }
}

/// Claude Code's folder (`~/.claude`, or `CLAUDE_CONFIG_DIR`).
fn claude_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|d| !d.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| crate::sandbox::home().map(|h| h.join(".claude")))
}

/// When the home folder is confined: Claude Code may use its folder and
/// settings, but of the conversations kept there only this one (see
/// [`crate::sandbox::own_claude_conversation`]).
fn own_conversation(policy: &mut crate::sandbox::Policy, cwd: &std::path::Path, id: &str) {
    if let (Some(dir), Some(home)) = (claude_dir(), crate::sandbox::home()) {
        crate::sandbox::own_claude_conversation(policy, &dir, &home, cwd, id);
    }
}

pub(super) async fn run_turn(
    hub: &Arc<AgentHub>,
    session: &Session,
    prompt: &str,
    system: &str,
    uuid: Option<&str>,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    let cfg = hub.config_for(session);
    let exe = super::agent_exe(&cfg, AgentKind::ClaudeCode)?;
    let dir = hub.session_dir(&session.id)?;
    let system_file = dir.join("system.md");
    std::fs::write(&system_file, system)?;

    // A rewind (see `AgentHub::rewind`) says where this turn picks up.
    let rewind = take_rewind(&dir);
    let resume = match &rewind {
        Some(Rewind::Fresh) => None,
        _ => session.agent_session_id.clone(),
    };
    // Claude Code's own conversation id, chosen here on the first turn so the
    // sandbox can let the chat into its transcript and no other.
    let agent_id = resume
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let mut policy = hub.policy(session);
    own_conversation(&mut policy, std::path::Path::new(&session.cwd), &agent_id);
    let mut settings = crate::sandbox::current().claude_settings(&policy);
    // A project's coordinator reads outside content (bug reports, email)
    // with access other chats lack, so it asks before any shell command (a
    // deploy, a payment CLI) and web fetch; `gh` and `git` already ask
    // before merging or pushing.
    let coordinator =
        super::coordinator::project_of(hub.store.session_key(&session.id)?.as_deref()).is_some();
    if coordinator {
        settings["permissions"]["allow"] = json!(if policy.brokered {
            vec!["Bash(gh:*)", "Bash(git:*)"]
        } else {
            Vec::new()
        });
        // A fetched page can carry data out in its URL. On a turn tab
        // started itself, nobody is reading along, so searches ask too.
        let mut ask = vec!["WebFetch"];
        if hub.is_woken(&session.id) {
            ask.push("WebSearch");
        }
        settings["permissions"]["ask"] = json!(ask);
    }
    // A copy to look at when debugging; Claude Code gets the JSON itself.
    // Given a file, it vets every folder on the path first and refuses the
    // file when one can't be examined, which happens under the sandbox.
    std::fs::write(
        dir.join("settings.json"),
        serde_json::to_vec_pretty(&settings)?,
    )?;
    let strict = strict(cfg.strict, policy.trust);
    let mut cmd = super::Args::default();
    cmd.arg("--settings").arg(serde_json::to_string(&settings)?);
    cmd.args(isolation_args(cfg.strict, policy.trust));
    cmd.args([
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
    ]);
    // The coordinator's asks reach the person whatever mode other chats use.
    let mode = if coordinator {
        "default"
    } else if MODES.contains(&cfg.permission_mode.as_str()) {
        cfg.permission_mode.as_str()
    } else {
        "default"
    };
    cmd.args(["--permission-mode", mode]);
    cmd.arg("--append-system-prompt-file").arg(&system_file);
    // Images the person attached live here; reading them needs no prompt.
    let attachments = hub.attachments_dir();
    if attachments.is_dir() {
        cmd.arg("--add-dir").arg(&attachments);
    }
    let remote = hub.remote_servers(policy.trust);
    // A cloud runner fronts only the connections the person allowed there:
    // no tab tools, and no approvals (nobody is at its keyboard).
    let own = !hub.remote_mcp().in_cloud();
    if let Some(mcp) = hub.mcp().filter(|_| own || !remote.is_empty()) {
        let mcp_file = dir.join("mcp.json");
        let headers = json!({
            "Authorization": format!("Bearer {}", crate::mcp::chat_token(&mcp.token, &session.id)),
            "X-Tab-Session": session.id,
        });
        let mut servers = serde_json::Map::new();
        if own {
            servers.insert(
                "tab".into(),
                json!({ "type": "http", "url": mcp.url, "headers": headers }),
            );
        }
        // Connected remote servers, through tab, which signs them in.
        for (name, url) in &remote {
            servers.insert(
                name.clone(),
                json!({ "type": "http", "url": url, "headers": headers }),
            );
        }
        let config = json!({ "mcpServers": servers });
        std::fs::write(&mcp_file, serde_json::to_vec_pretty(&config)?)?;
        cmd.arg("--mcp-config").arg(&mcp_file);
        // Approvals come to tab; tab's own tools need none, and tab asks
        // about remote tools itself, by the person's choice for each.
        if own {
            cmd.args(["--permission-prompt-tool", "mcp__tab__approve"]);
        }
        cmd.arg("--allowedTools");
        if own {
            cmd.arg("mcp__tab");
        }
        for (name, _) in &remote {
            cmd.arg(format!("mcp__{name}"));
        }
    }
    if let Some(m) = cfg.model_for(AgentKind::ClaudeCode) {
        cmd.args(["--model", m]);
    }
    if let Some(cap) = cfg.budget.per_turn_usd.filter(|c| *c > 0.0) {
        cmd.args(["--max-budget-usd", &format!("{cap:.2}")]);
    }
    match &resume {
        Some(id) => cmd.args(["--resume", id]),
        None => cmd.args(["--session-id", &agent_id]),
    };
    if let (Some(_), Some(Rewind::At(at))) = (&resume, &rewind) {
        cmd.args(["--resume-session-at", at]);
    }
    let mut prepared = crate::sandbox::current()
        .command(&exe, &cmd.0, std::path::Path::new(&session.cwd), &policy)
        .await?;
    hub.add_shims(session, &mut prepared.cmd);
    // Claude Code backs up each file before editing it, per message, so a
    // rewind can put the code back too.
    prepared
        .cmd
        .env("CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING", "1");
    if strict {
        // claude.ai connectors too, should a version load them despite
        // `--strict-mcp-config`.
        prepared.cmd.env("ENABLE_CLAUDEAI_MCP_SERVERS", "false");
    }
    prepared
        .cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    tracing::debug!(session = %session.id, mode, strict, resume = ?session.agent_session_id, sandboxed = prepared.enforced, "starting Claude Code turn");
    let sandboxed = prepared.enforced;
    let mut child = prepared
        .spawn()
        .map_err(|e| anyhow::anyhow!("starting Claude Code ({}) failed: {e}", exe.display()))?;
    // stdin stays open so the person can steer mid-turn; it closes when the
    // turn's result arrives (see `steering_close`) and the queue drains.
    let mut stdin = child.stdin.take().expect("piped");
    // `initialize` first, as the SDK does: its answer describes the slash
    // commands (what they do, what arguments they take) for the `/` menu.
    let init = json!({ "type": "control_request", "request_id": INIT_REQUEST, "request": { "subtype": "initialize" } });
    let mut first = json!({ "type": "user", "message": { "role": "user", "content": prompt } });
    // tab's id for the message, so a rewind can name it later.
    if let Some(u) = uuid {
        first["uuid"] = json!(u);
    }
    stdin
        .write_all(format!("{init}\n{first}\n").as_bytes())
        .await?;
    let mut steer_rx = hub.steering_open(&session.id);
    let open_tools = OpenTools::default();
    let tools = open_tools.clone();
    let unanswered = Arc::new(Mutex::new(Unanswered::default()));
    let asked = unanswered.clone();
    tokio::spawn(async move {
        let mut n = 0;
        while let Some(steer) = steer_rx.recv().await {
            n += 1;
            if let Steer::Message(text) = &steer {
                asked.lock().unwrap().sent(text);
            }
            let running = !tools.lock().unwrap().is_empty();
            let Some(line) = steer_line(steer, running, n) else {
                continue;
            };
            if stdin
                .write_all(format!("{line}\n").as_bytes())
                .await
                .is_err()
            {
                break;
            }
        }
    });

    let stdout = child.stdout.take().expect("piped");
    let mut stderr = child.stderr.take().expect("piped");
    let err_task = tokio::spawn(async move {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s).await;
        s
    });

    let mut lines = BufReader::new(stdout).lines();
    let mut parser = Parser::new(hub.clone(), session.id.clone());
    parser.open_tools = open_tools;
    parser.unanswered = unanswered;
    parser.strict = strict;
    parser.cwd = Some(session.cwd.clone().into());
    parser.allowed_mcp = remote.into_iter().map(|(name, _)| name).collect();
    // Rough size of what tab adds to the system prompt (the item and its context).
    parser.tab_context_tokens = (system.len() / 4) as i64;
    // After the result the CLI normally exits right away. If it stays up,
    // it's waiting on background work it started, so the chat is monitoring.
    let mut monitoring = false;
    loop {
        let settle = async {
            if parser.finished && !parser.failed && !monitoring {
                tokio::time::sleep(std::time::Duration::from_millis(1500)).await
            } else {
                std::future::pending::<()>().await
            }
        };
        tokio::select! {
            _ = settle => {
                monitoring = true;
                hub.update(&session.id, Some(SessionStatus::Monitoring), None);
            }
            line = lines.next_line() => match line? {
                Some(l) if !l.trim().is_empty() => match serde_json::from_str::<Value>(&l) {
                    Ok(v) => {
                        parser.handle(&v);
                        if !parser.leaked.is_empty() {
                            child.kill_tree();
                            anyhow::bail!(
                                "Strict mode stopped this turn: Claude Code loaded {}, which isn't one of tab's connections.",
                                parser.leaked.join(", ")
                            );
                        }
                        // Background work woke the agent up: it's working again.
                        if !parser.finished {
                            monitoring = false;
                        }
                    }
                    Err(_) => tracing::debug!(line = %l, "non-json output from claude"),
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
    if monitoring {
        hub.update(&session.id, Some(SessionStatus::Idle), None);
    }
    let status = child.wait().await?;
    let stderr = err_task.await.unwrap_or_default();
    if !parser.finished {
        anyhow::bail!(
            "Claude Code exited ({status}) before finishing.{}{}",
            super::per_turn::tail(&stderr),
            startup_hint(&stderr, sandboxed)
        );
    }
    Ok(())
}

/// Claude Code's runtime (Bun) blames the open-file limit for any error it
/// can't name while starting, and suggests `ulimit`. Under tab's sandbox the
/// usual cause is a folder it was kept out of, which no limit fixes.
fn startup_hint(stderr: &str, sandboxed: bool) -> &'static str {
    let fd_message =
        stderr.contains("max file descriptors") || stderr.contains("ran out of file descriptors");
    if fd_message && sandboxed {
        "\n\ntab's sandbox may have kept Claude Code out of something it needs. Turning off \"Only what's needed\" in Settings → Sandbox works around it."
    } else {
        ""
    }
}

/// Turns stream-json lines into transcript entries.
struct Parser {
    hub: Arc<AgentHub>,
    session_id: String,
    seen: HashSet<String>,
    /// Ids of calls that create something linkable, so their output can be attached as Context.
    create_calls: HashSet<String>,
    finished: bool,
    /// The last result was an error.
    failed: bool,
    billing: &'static str,
    /// Usage of the first and latest model call this turn, for the context meter.
    first_call: Option<Value>,
    last_call: Option<Value>,
    tab_context_tokens: i64,
    /// Shared with the steering task, which interrupts only while one runs.
    open_tools: OpenTools,
    /// The person's mid-turn messages, until answered (shared with the
    /// steering task, which notes each one it sends).
    unanswered: Arc<Mutex<Unanswered>>,
    /// The person pressed "Send now": the next result only closes the
    /// interrupted part, and the turn goes on with their queued message.
    interrupted: bool,
    /// Strict mode: only tab's MCP server may load.
    strict: bool,
    /// What loaded anyway, from the init line. Non-empty stops the turn.
    leaked: Vec<String>,
    /// Remote MCP servers tab added for this chat (strict allows them).
    allowed_mcp: Vec<String>,
    /// Open calls that only wait on something outside (CI, a deploy):
    /// while any runs the chat shows Monitoring, not Working.
    waits: HashSet<String>,
    /// The chat's folder, where its project's custom commands are.
    cwd: Option<std::path::PathBuf>,
    /// The id of the agent's latest reply: a rewind to the message after
    /// this turn resumes the conversation from there.
    last_reply: Option<String>,
}

/// Where the next turn of a rewound chat picks up its conversation.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Rewind {
    /// After the agent's reply with this id; what came later is dropped.
    At(String),
    /// From nothing: the chat went back to before its first message.
    Fresh,
}

const REWIND_FILE: &str = "rewind.json";

/// Leaves word for the chat's next turn to pick up from `rewind`.
pub(super) fn set_rewind(dir: &std::path::Path, rewind: &Rewind) -> anyhow::Result<()> {
    std::fs::write(dir.join(REWIND_FILE), serde_json::to_vec(rewind)?)?;
    Ok(())
}

/// The rewind left for this turn, if any; it applies once.
fn take_rewind(dir: &std::path::Path) -> Option<Rewind> {
    let path = dir.join(REWIND_FILE);
    let r = serde_json::from_slice(&std::fs::read(&path).ok()?).ok();
    let _ = std::fs::remove_file(path);
    r
}

/// What restoring files to before a message changes (or changed).
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct RewoundFiles {
    pub files: Vec<String>,
    pub insertions: i64,
    pub deletions: i64,
}

/// Puts the files the agent edited back as they were before the message
/// with `uuid`, from Claude Code's own backups (see the checkpointing env in
/// [`run_turn`]). With `dry_run` it only says what that would change. Runs
/// Claude Code on its own, under the chat's sandbox, with no prompt.
pub(super) async fn rewind_files(
    hub: &Arc<AgentHub>,
    session: &Session,
    uuid: &str,
    dry_run: bool,
) -> anyhow::Result<RewoundFiles> {
    let Some(agent_id) = session.agent_session_id.clone() else {
        anyhow::bail!("This chat hasn't started a conversation yet.");
    };
    let cfg = hub.config_for(session);
    let exe = super::agent_exe(&cfg, AgentKind::ClaudeCode)?;
    let mut policy = hub.policy(session);
    own_conversation(&mut policy, std::path::Path::new(&session.cwd), &agent_id);
    let settings = crate::sandbox::current().claude_settings(&policy);
    let mut cmd = super::Args::default();
    cmd.arg("--settings").arg(serde_json::to_string(&settings)?);
    cmd.args(isolation_args(cfg.strict, policy.trust));
    cmd.args([
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--resume",
        &agent_id,
    ]);
    let mut prepared = crate::sandbox::current()
        .command(&exe, &cmd.0, std::path::Path::new(&session.cwd), &policy)
        .await?;
    prepared
        .cmd
        .env("CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = prepared
        .spawn()
        .map_err(|e| anyhow::anyhow!("starting Claude Code ({}) failed: {e}", exe.display()))?;
    let mut stdin = child.stdin.take().expect("piped");
    let req = json!({
        "type": "control_request",
        "request_id": "tab-rewind",
        "request": { "subtype": "rewind_files", "user_message_id": uuid, "dry_run": dry_run },
    });
    stdin.write_all(format!("{req}\n").as_bytes()).await?;
    let mut lines = BufReader::new(child.stdout.take().expect("piped")).lines();
    let answer = tokio::time::timeout(std::time::Duration::from_secs(60), async {
        while let Some(l) = lines.next_line().await? {
            let Ok(v) = serde_json::from_str::<Value>(&l) else {
                continue;
            };
            if v["type"] == "control_response" && v["response"]["request_id"] == "tab-rewind" {
                return anyhow::Ok(Some(v["response"].clone()));
            }
        }
        anyhow::Ok(None)
    })
    .await;
    drop(stdin);
    child.kill_tree();
    let r = match answer {
        Ok(r) => {
            r?.ok_or_else(|| anyhow::anyhow!("Claude Code exited without answering the rewind."))?
        }
        Err(_) => anyhow::bail!("Claude Code didn't answer the rewind within a minute."),
    };
    rewound(&r)
}

/// The answer to a `rewind_files` request.
fn rewound(r: &Value) -> anyhow::Result<RewoundFiles> {
    if r["subtype"] == "error" {
        anyhow::bail!(
            "{}",
            r["error"]
                .as_str()
                .unwrap_or("Claude Code couldn't rewind the files.")
        );
    }
    let body = &r["response"];
    if body["canRewind"] == false {
        anyhow::bail!(
            "{}",
            body["error"]
                .as_str()
                .unwrap_or("Claude Code has no file backups for that message.")
        );
    }
    Ok(RewoundFiles {
        files: body["filesChanged"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|f| f.as_str().map(str::to_string))
            .collect(),
        insertions: body["insertions"].as_i64().unwrap_or(0),
        deletions: body["deletions"].as_i64().unwrap_or(0),
    })
}

/// Whether a chat runs strict: when the person chose it, and always for an
/// agent working on someone else's code.
fn strict(chosen: bool, trust: Trust) -> bool {
    chosen || trust != Trust::Person
}

/// What keeps the person's own Claude Code setup out of a chat.
///
/// Strict: only `--mcp-config` servers (tab's), and only the repo's settings
/// (`project`, `local`): no user MCP servers, claude.ai connectors, user
/// hooks, plugins, allow rules or `~/.claude/CLAUDE.md`. Code nobody here
/// wrote (a pull request's checkout) gets no settings at all, since its
/// `.claude/settings.json` hooks would run as soon as the chat starts.
fn isolation_args(chosen: bool, trust: Trust) -> Vec<&'static str> {
    match (strict(chosen, trust), trust) {
        (false, _) => Vec::new(),
        (true, Trust::Person) => vec!["--strict-mcp-config", "--setting-sources", "project,local"],
        (true, _) => vec!["--strict-mcp-config", "--setting-sources", ""],
    }
}

/// MCP servers and account plugins in Claude Code's init line that strict
/// mode doesn't allow: anything but tab's own server and the remote servers
/// tab added (`allowed`).
fn unexpected(init: &Value, allowed: &[String]) -> Vec<String> {
    let servers = init["mcp_servers"].as_array().into_iter().flatten();
    let plugins = init["plugins"].as_array().into_iter().flatten();
    servers
        .filter_map(|s| s["name"].as_str())
        .filter(|n| *n != "tab" && !allowed.iter().any(|a| a == n))
        .map(|n| format!("MCP server \"{n}\""))
        .chain(
            plugins
                .filter(|p| p["source"].as_str().is_some_and(|s| s.ends_with("@synced")))
                .filter_map(|p| p["name"].as_str())
                .map(|n| format!("plugin \"{n}\"")),
        )
        .collect()
}

/// The MCP servers in Claude Code's init line, with their status.
fn mcp_servers(init: &Value) -> Vec<super::AgentMcpServer> {
    init["mcp_servers"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| {
            Some(super::AgentMcpServer {
                name: s["name"].as_str()?.to_string(),
                status: s["status"].as_str().map(str::to_string),
            })
        })
        .collect()
}

/// Everything a model call read: new input plus cache writes and reads.
fn prompt_tokens(u: &Value) -> i64 {
    [
        "input_tokens",
        "cache_creation_input_tokens",
        "cache_read_input_tokens",
    ]
    .iter()
    .map(|k| u[*k].as_i64().unwrap_or(0))
    .sum()
}

impl Parser {
    fn new(hub: Arc<AgentHub>, session_id: String) -> Self {
        Self {
            hub,
            session_id,
            seen: HashSet::new(),
            create_calls: HashSet::new(),
            finished: false,
            failed: false,
            billing: "unknown",
            first_call: None,
            last_call: None,
            tab_context_tokens: 0,
            open_tools: OpenTools::default(),
            unanswered: Default::default(),
            interrupted: false,
            strict: false,
            leaked: Vec::new(),
            allowed_mcp: Vec::new(),
            waits: HashSet::new(),
            cwd: None,
            last_reply: None,
        }
    }

    fn save_commands(&self, cmds: &Value) {
        let cmds: Vec<super::SlashCommand> = cmds
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|c| {
                let name = c["name"].as_str()?.to_string();
                let description = c["description"].as_str().unwrap_or_default().to_string();
                // Commands only a server-launched session can run.
                if name.starts_with("__") {
                    return None;
                }
                Some(super::SlashCommand {
                    kind: self.slash_kind(
                        &name,
                        c["builtin"].as_bool().unwrap_or(false),
                        &description,
                    ),
                    argument_hint: c["argumentHint"].as_str().unwrap_or_default().to_string(),
                    aliases: c["aliases"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|a| a.as_str().map(str::to_string))
                        .collect(),
                    name,
                    description,
                })
            })
            .collect();
        self.hub.save_slash_commands(AgentKind::ClaudeCode, &cmds);
    }

    /// Where a command comes from. Claude Code says only whether it's its
    /// own; a custom command is a file in a `commands` folder (`a:b` is
    /// `a/b.md`), and anything else is a skill.
    fn slash_kind(&self, name: &str, builtin: bool, description: &str) -> super::SlashKind {
        use super::SlashKind;
        if builtin {
            return SlashKind::Builtin;
        }
        if name.starts_with("mcp__") || description.ends_with("(MCP)") {
            return SlashKind::Mcp;
        }
        let file = format!("{}.md", name.replace(':', "/"));
        let mut dirs: Vec<std::path::PathBuf> = claude_dir().into_iter().collect();
        dirs.extend(self.cwd.iter().map(|c| c.join(".claude")));
        if dirs
            .iter()
            .any(|d| d.join("commands").join(&file).is_file())
        {
            SlashKind::Command
        } else {
            SlashKind::Skill
        }
    }

    /// How full the context window is after this turn, and what fills it.
    fn context(&self, result: &Value) -> Value {
        let Some(last) = &self.last_call else {
            return Value::Null;
        };
        let (model, window) = result["modelUsage"]
            .as_object()
            .and_then(|m| {
                m.iter()
                    .max_by_key(|(_, u)| u["outputTokens"].as_i64().unwrap_or(0))
                    .map(|(k, u)| (k.clone(), u["contextWindow"].as_i64()))
            })
            .unwrap_or_default();
        json!({
            "model": model,
            "window": window.unwrap_or(200_000),
            // The next turn starts from everything the last call read plus what it wrote.
            "used": prompt_tokens(last) + last["output_tokens"].as_i64().unwrap_or(0),
            "first_call": self.first_call.as_ref().map(prompt_tokens),
            "cache_read": last["cache_read_input_tokens"],
            "cache_write": last["cache_creation_input_tokens"],
            "fresh_input": last["input_tokens"],
            "output": last["output_tokens"],
            "tab_context": self.tab_context_tokens,
        })
    }

    fn once(&mut self, key: String) -> bool {
        self.seen.insert(key)
    }

    fn handle(&mut self, v: &Value) {
        let sid = self.session_id.clone();
        let hub = self.hub.clone();
        let kind = v["type"].as_str().unwrap_or_default();
        // Output after the result means background work (a finished
        // background command, a watcher firing) started a new turn.
        if self.finished && matches!(kind, "assistant" | "stream_event") {
            self.finished = false;
            hub.update(&sid, Some(SessionStatus::Working), None);
        }
        match kind {
            "system" if v["subtype"] == "init" => {
                if self.strict {
                    self.leaked = unexpected(v, &self.allowed_mcp);
                } else {
                    // Their settings are out of reach; their names say
                    // what Connections can offer to bring over.
                    let names: Vec<String> = v["mcp_servers"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|s| s["name"].as_str())
                        .filter(|n| n.starts_with("claude.ai "))
                        .map(str::to_string)
                        .collect();
                    hub.save_claude_ai_connectors(&names);
                }
                self.billing = crate::usage::billing_from_init(v);
                hub.save_agent_mcp(&sid, &mcp_servers(v));
                if let Some(cmds) = v["slash_commands"].as_array() {
                    let names: Vec<String> = cmds
                        .iter()
                        .filter_map(|c| c.as_str().map(str::to_string))
                        .collect();
                    hub.add_slash_names(&names, |n| self.slash_kind(n, false, ""));
                }
                if let Some(id) = v["session_id"].as_str() {
                    hub.update(&sid, None, Some(id));
                }
            }
            // The answer to `initialize` (see `run_turn`), and the list
            // again when it changes mid-turn: commands with what they do.
            "control_response" if v["response"]["request_id"] == INIT_REQUEST => {
                let r = &v["response"]["response"];
                self.save_commands(&r["commands"]);
                let agents: Vec<super::Subagent> = r["agents"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|a| {
                        Some(super::Subagent {
                            name: a["name"].as_str()?.to_string(),
                            description: a["description"].as_str().unwrap_or_default().to_string(),
                        })
                    })
                    .collect();
                hub.save_subagents(&agents);
            }
            "system" if v["subtype"] == "commands_changed" => self.save_commands(&v["commands"]),
            "stream_event" => {
                let ev = &v["event"];
                if ev["type"] == "content_block_delta"
                    && ev["delta"]["type"] == "text_delta"
                    && let Some(t) = ev["delta"]["text"].as_str()
                {
                    hub.delta(&sid, t);
                }
            }
            "assistant" => {
                if let Some(u) = v["uuid"].as_str() {
                    self.last_reply = Some(u.to_string());
                }
                let msg_id = v["message"]["id"].as_str().unwrap_or_default().to_string();
                let usage = &v["message"]["usage"];
                if usage.is_object() && prompt_tokens(usage) > 0 {
                    if self.first_call.is_none() {
                        self.first_call = Some(usage.clone());
                    }
                    self.last_call = Some(usage.clone());
                }
                for (i, block) in v["message"]["content"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .enumerate()
                {
                    match block["type"].as_str() {
                        Some("text") => {
                            let text = block["text"].as_str().unwrap_or_default();
                            if !text.trim().is_empty() && self.once(format!("{msg_id}:{i}:{text}"))
                            {
                                self.unanswered.lock().unwrap().replied();
                                hub.post(&sid, "assistant", "text", text, Value::Null);
                            }
                        }
                        Some("tool_use") => {
                            let id = block["id"].as_str().unwrap_or_default().to_string();
                            if self.once(format!("tool:{id}")) {
                                let skipped = self.unanswered.lock().unwrap().skipped();
                                if !skipped.is_empty() {
                                    hub.post(
                                        &sid,
                                        "system",
                                        "status",
                                        "The agent went on without replying to your message; tab asked it to answer.",
                                        Value::Null,
                                    );
                                    hub.remind(&sid, reminder(&skipped));
                                }
                                self.open_tools.lock().unwrap().insert(id.clone());
                                let name = block["name"].as_str().unwrap_or("tool");
                                if super::is_wait(name, &block["input"]) {
                                    self.waits.insert(id.clone());
                                    hub.update(&sid, Some(SessionStatus::Monitoring), None);
                                }
                                if crate::links::is_creator(name, &block["input"]) {
                                    self.create_calls.insert(id.clone());
                                }
                                hub.post(
                                    &sid,
                                    "assistant",
                                    "tool",
                                    &super::summarize_tool(name, &block["input"]),
                                    json!({ "tool": name, "id": id, "input": block["input"] }),
                                );
                            }
                        }
                        _ => {}
                    }
                }
            }
            "user" => {
                for block in v["message"]["content"].as_array().into_iter().flatten() {
                    if block["type"] == "text"
                        && block["text"]
                            .as_str()
                            .is_some_and(|t| t.starts_with("[Request interrupted by user"))
                    {
                        self.interrupted = true;
                    }
                    if block["type"] == "tool_result" {
                        self.unanswered.lock().unwrap().shown();
                        if let Some(id) = block["tool_use_id"].as_str() {
                            self.open_tools.lock().unwrap().remove(id);
                            if self.waits.remove(id) && self.waits.is_empty() {
                                hub.update(&sid, Some(SessionStatus::Working), None);
                            }
                        }
                        let text = match &block["content"] {
                            Value::String(s) => s.clone(),
                            Value::Array(parts) => parts
                                .iter()
                                .filter_map(|p| p["text"].as_str())
                                .collect::<Vec<_>>()
                                .join("\n"),
                            _ => String::new(),
                        };
                        if block["tool_use_id"]
                            .as_str()
                            .is_some_and(|i| self.create_calls.remove(i))
                        {
                            hub.attach_created_links(&sid, &text);
                        }
                        hub.post(
                            &sid,
                            "tool",
                            "tool_result",
                            &text.chars().take(4000).collect::<String>(),
                            json!({
                                "tool_use_id": block["tool_use_id"],
                                "is_error": block["is_error"],
                            }),
                        );
                    }
                }
            }
            "rate_limit_event" => {
                let info = &v["rate_limit_info"];
                crate::usage::record_rate_limit(&hub.store, info);
                if info["status"]
                    .as_str()
                    .is_some_and(|s| !s.starts_with("allowed"))
                {
                    hub.post(
                        &sid,
                        "system",
                        "status",
                        &format!(
                            "Claude usage limit: {} ({})",
                            info["status"].as_str().unwrap_or_default(),
                            info["rateLimitType"].as_str().unwrap_or("limit")
                        ),
                        info.clone(),
                    );
                }
            }
            "result" if std::mem::take(&mut self.interrupted) => {
                // Interrupted to read steering that is already queued:
                // count the cost and keep going.
                self.open_tools.lock().unwrap().clear();
                crate::usage::record_claude_result(
                    &hub.store,
                    v,
                    "agent",
                    Some(&sid),
                    self.billing,
                );
            }
            "result" => {
                self.finished = true;
                self.open_tools.lock().unwrap().clear();
                hub.steering_close(&sid);
                crate::usage::record_claude_result(
                    &hub.store,
                    v,
                    "agent",
                    Some(&sid),
                    self.billing,
                );
                if let Some(id) = v["session_id"].as_str() {
                    hub.update(&sid, None, Some(id));
                }
                let failed = v["is_error"].as_bool().unwrap_or(false);
                self.failed = failed;
                if failed {
                    let msg = v["result"]
                        .as_str()
                        .or(v["error"].as_str())
                        .unwrap_or("The turn failed");
                    hub.post(&sid, "system", "error", msg, Value::Null);
                }
                hub.post(
                    &sid,
                    "system",
                    "result",
                    "",
                    json!({
                        "cost_usd": v["total_cost_usd"],
                        "duration_ms": v["duration_ms"],
                        "turns": v["num_turns"],
                        "stop_reason": v["stop_reason"],
                        "context": self.context(v),
                        // Where a rewind to the next message resumes.
                        "agent_uuid": self.last_reply,
                    }),
                );
                hub.update(
                    &sid,
                    Some(if failed {
                        SessionStatus::Failed
                    } else {
                        SessionStatus::Idle
                    }),
                    None,
                );
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AgentConfig;
    use crate::engine::EngineEvent;
    use crate::store::Store;
    use chrono::Utc;

    /// Bun's startup error, as Claude Code prints it on macOS.
    const BUN_STARTUP: &str = "error: An unknown error occurred, possibly due to low max file descriptors (Unexpected)\n\nCurrent limit: 256\n\nTo fix this, try running:\n\n  ulimit -n 2147483646\n\nIf that still doesn't work, you may need to run:\n\n  sudo launchctl limit maxfiles 2147483646\n";

    #[test]
    fn a_startup_failure_keeps_its_first_line_and_points_at_the_sandbox() {
        let tail = super::super::per_turn::tail(BUN_STARTUP);
        assert!(tail.contains("(Unexpected)"), "{tail}");
        assert!(tail.contains("Current limit: 256"), "{tail}");
        assert!(startup_hint(BUN_STARTUP, true).contains("Only what's needed"));
        assert_eq!(startup_hint(BUN_STARTUP, false), "");
        assert_eq!(startup_hint("error: not signed in", true), "");
    }

    /// The real Claude Code in a confined chat: it starts, signs in, keeps
    /// its transcript, and its shell can't list the Desktop. Run by hand
    /// from a terminal (macOS can't start a sandbox inside tab's):
    /// `cargo test -p tab-core --lib real_claude_in_a_confined_chat -- --ignored --nocapture`
    #[cfg(target_os = "macos")]
    #[tokio::test]
    #[ignore]
    async fn real_claude_in_a_confined_chat() {
        let sb = crate::sandbox::current();
        assert!(
            sb.backend().enforces(),
            "run this outside a sandbox ({:?})",
            sb.backend()
        );
        let exe = which::which("claude").expect("claude on PATH");
        let cwd = crate::sandbox::real(&std::env::temp_dir())
            .join(format!("tab-confined-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&cwd).unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let mut p = sb.chat_policy();
        own_conversation(&mut p, &cwd, &id);
        let args: Vec<std::ffi::OsString> = [
            "-p",
            "Run `ls ~/Desktop` with Bash, then answer with the word DONE and what it printed.",
            "--session-id",
            &id,
            "--allowedTools",
            "Bash",
        ]
        .iter()
        .map(Into::into)
        .collect();
        let mut c = sb.command(&exe, &args, &cwd, &p).await.unwrap();
        c.cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        let out = c.cmd.output().await.unwrap();
        let text = String::from_utf8_lossy(&out.stdout).to_string()
            + &String::from_utf8_lossy(&out.stderr);
        println!("{text}");
        assert!(out.status.success() && text.contains("DONE"), "{text}");
        let transcript = claude_dir()
            .unwrap()
            .join("projects")
            .join(crate::sandbox::claude_project_slug(&cwd))
            .join(format!("{id}.jsonl"));
        assert!(
            transcript.is_file(),
            "no transcript at {}",
            transcript.display()
        );
    }

    #[test]
    fn a_confined_chat_reaches_only_its_own_conversation() {
        let root = crate::sandbox::real(&std::env::temp_dir())
            .join(format!("tab-claude-home-{}", uuid::Uuid::new_v4()));
        let (dir, cwd) = (root.join(".claude"), std::path::Path::new("/work/my repo"));
        let mut p = crate::sandbox::current().chat_policy();
        p.confine.clear();
        crate::sandbox::own_claude_conversation(&mut p, &dir, &root, cwd, "abc");
        assert!(
            p.reveal_rw.is_empty(),
            "nothing changes when the home isn't confined"
        );

        p.confine = vec![root.clone()];
        crate::sandbox::own_claude_conversation(&mut p, &dir, &root, cwd, "abc");
        let project = dir.join("projects").join("-work-my-repo");
        assert!(project.is_dir(), "made before the agent starts");
        assert!(p.open.contains(&dir));
        assert!(p.open.contains(&root.join(".claude.json*")));
        for hidden in ["projects", "history.jsonl", "file-history", "todos"] {
            assert!(p.hide.contains(&dir.join(hidden)), "{hidden}");
        }
        assert!(p.reveal_rw.contains(&project.join("abc.jsonl")));
        assert!(p.reveal_rw.contains(&project.join("abc")));
        assert!(p.reveal_rw.contains(&project.join("memory")));
        assert!(
            !p.reveal_rw
                .iter()
                .any(|r| r.ends_with("projects") || r == &project)
        );
        // Claude Code's Read rules can't carve out the transcript, so the
        // sandbox alone holds the projects folder.
        let rules = crate::sandbox::current().claude_settings(&p);
        let deny = rules["permissions"]["deny"].to_string();
        assert!(!deny.contains("projects"), "{deny}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn parses_a_stream_json_turn() {
        let store = Arc::new(Store::in_memory().unwrap());
        let (tx, mut rx) = tokio::sync::broadcast::channel(64);
        let hub = AgentHub::new(
            store.clone(),
            tx,
            AgentConfig::default(),
            std::env::temp_dir(),
        );
        let s = store
            .session_create(None, AgentKind::ClaudeCode, "t", ".", Utc::now())
            .unwrap();
        let mut p = Parser::new(hub, s.id.clone());
        for line in [
            r#"{"type":"system","subtype":"init","session_id":"abc"}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"Hel"}}}"#,
            r#"{"type":"assistant","message":{"id":"m1","content":[{"type":"text","text":"Hello"}]}}"#,
            r#"{"type":"assistant","message":{"id":"m1","content":[{"type":"text","text":"Hello"}]}}"#,
            r#"{"type":"assistant","message":{"id":"m2","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"ls"}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"a.txt"}]}}"#,
            r#"{"type":"result","session_id":"abc","is_error":false,"total_cost_usd":0.01}"#,
        ] {
            p.handle(&serde_json::from_str(line).unwrap());
        }
        assert!(p.finished);
        let msgs = store.messages(&s.id).unwrap();
        let kinds: Vec<_> = msgs.iter().map(|m| m.kind.as_str()).collect();
        assert_eq!(kinds, ["text", "tool", "tool_result", "result"]);
        assert_eq!(msgs[1].content, "Bash: ls");
        let s = store.session_get(&s.id).unwrap().unwrap();
        assert_eq!(
            (s.agent_session_id.as_deref(), s.status),
            (Some("abc"), SessionStatus::Idle)
        );
        let mut deltas = 0;
        while let Ok(e) = rx.try_recv() {
            if matches!(e, EngineEvent::SessionDelta { .. }) {
                deltas += 1;
            }
        }
        assert_eq!(deltas, 1);
    }

    #[tokio::test]
    async fn the_result_names_the_last_reply_for_a_rewind() {
        let store = Arc::new(Store::in_memory().unwrap());
        let (tx, _rx) = tokio::sync::broadcast::channel(64);
        let hub = AgentHub::new(
            store.clone(),
            tx,
            AgentConfig::default(),
            std::env::temp_dir(),
        );
        let s = store
            .session_create(None, AgentKind::ClaudeCode, "t", ".", Utc::now())
            .unwrap();
        let mut p = Parser::new(hub, s.id.clone());
        for line in [
            r#"{"type":"assistant","uuid":"a1","message":{"id":"m1","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"ls"}}]}}"#,
            r#"{"type":"user","uuid":"u2","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"a.txt"}]}}"#,
            r#"{"type":"assistant","uuid":"a2","message":{"id":"m2","content":[{"type":"text","text":"Done"}]}}"#,
            r#"{"type":"result","session_id":"abc","is_error":false}"#,
        ] {
            p.handle(&serde_json::from_str(line).unwrap());
        }
        let msgs = store.messages(&s.id).unwrap();
        assert_eq!(msgs.last().unwrap().meta["agent_uuid"], "a2");
    }

    #[test]
    fn a_rewind_applies_to_one_turn() {
        let dir = std::env::temp_dir().join(format!("tab-rewind-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(take_rewind(&dir), None);
        set_rewind(&dir, &Rewind::At("a2".into())).unwrap();
        assert_eq!(take_rewind(&dir), Some(Rewind::At("a2".into())));
        assert_eq!(take_rewind(&dir), None);
        set_rewind(&dir, &Rewind::Fresh).unwrap();
        assert_eq!(take_rewind(&dir), Some(Rewind::Fresh));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn rewind_answers_say_what_changed_or_why_not() {
        let ok = json!({ "subtype": "success", "response": { "canRewind": true, "filesChanged": ["/r/a.txt"], "insertions": 1, "deletions": 2 } });
        let r = rewound(&ok).unwrap();
        assert_eq!(
            (r.files, r.insertions, r.deletions),
            (vec!["/r/a.txt".to_string()], 1, 2)
        );
        let none = json!({ "subtype": "success", "response": { "canRewind": false, "error": "No file checkpoint found for this message." } });
        assert_eq!(
            rewound(&none).unwrap_err().to_string(),
            "No file checkpoint found for this message."
        );
        let err = json!({ "subtype": "error", "error": "File rewinding is not enabled." });
        assert_eq!(
            rewound(&err).unwrap_err().to_string(),
            "File rewinding is not enabled."
        );
    }

    /// A parser and the turn's steering channel, as `run_turn` sets them up.
    fn steered_turn() -> (
        Arc<Store>,
        String,
        Parser,
        tokio::sync::mpsc::UnboundedReceiver<Steer>,
    ) {
        let store = Arc::new(Store::in_memory().unwrap());
        let hub = AgentHub::new(
            store.clone(),
            tokio::sync::broadcast::channel(64).0,
            AgentConfig::default(),
            std::env::temp_dir(),
        );
        let s = store
            .session_create(None, AgentKind::ClaudeCode, "t", ".", Utc::now())
            .unwrap();
        let rx = hub.steering_open(&s.id);
        let p = Parser::new(hub, s.id.clone());
        (store, s.id, p, rx)
    }

    fn feed(p: &mut Parser, lines: &[&str]) {
        for line in lines {
            p.handle(&serde_json::from_str(line).unwrap());
        }
    }

    const CALL_1: &str = r#"{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"ls"}}]}}"#;
    const RESULT_1: &str = r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"a.txt"}]}}"#;
    const THINK_2: &str = r#"{"type":"assistant","message":{"id":"m2","content":[{"type":"thinking","thinking":"It's a made-up path."}]}}"#;
    const CALL_2: &str = r#"{"type":"assistant","message":{"id":"m2","content":[{"type":"tool_use","id":"t2","name":"Bash","input":{"command":"pwd"}}]}}"#;

    #[tokio::test]
    async fn a_message_answered_only_in_thinking_gets_the_agent_reminded() {
        let (store, sid, mut p, mut steering) = steered_turn();
        // Sent while the first call runs; the model sees it with its result.
        feed(&mut p, &[CALL_1]);
        p.unanswered.lock().unwrap().sent("what is shot.png?");
        feed(&mut p, &[RESULT_1, THINK_2, CALL_2]);
        let note = store
            .messages(&sid)
            .unwrap()
            .into_iter()
            .find(|m| m.kind == "status");
        assert!(note.is_some_and(|m| m.content.contains("without replying")));
        match steering.try_recv() {
            Ok(Steer::Reminder(r)) => {
                assert!(r.contains("\"what is shot.png?\""));
                assert!(r.contains("visible"));
            }
            _ => panic!("expected a reminder"),
        }
        // Once per message: the next call alone doesn't remind again.
        feed(
            &mut p,
            &[
                r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t2","content":"/w"}]}}"#,
                r#"{"type":"assistant","message":{"id":"m3","content":[{"type":"tool_use","id":"t3","name":"Bash","input":{"command":"ls"}}]}}"#,
            ],
        );
        assert!(steering.try_recv().is_err());
    }

    #[tokio::test]
    async fn a_message_answered_in_text_needs_no_reminder() {
        let (store, sid, mut p, mut steering) = steered_turn();
        feed(&mut p, &[CALL_1]);
        p.unanswered.lock().unwrap().sent("what is shot.png?");
        feed(
            &mut p,
            &[
                RESULT_1,
                THINK_2,
                r#"{"type":"assistant","message":{"id":"m2","content":[{"type":"text","text":"Nothing real: an example path."}]}}"#,
                CALL_2,
            ],
        );
        assert!(steering.try_recv().is_err());
        assert!(
            !store
                .messages(&sid)
                .unwrap()
                .iter()
                .any(|m| m.kind == "status")
        );
    }

    #[tokio::test]
    async fn a_call_made_before_the_message_was_shown_is_fine() {
        // The model was already writing its next call when the message
        // arrived: it only sees the message with that call's result.
        let (_store, _sid, mut p, mut steering) = steered_turn();
        p.unanswered.lock().unwrap().sent("what is shot.png?");
        feed(&mut p, &[CALL_1]);
        assert!(steering.try_recv().is_err());
    }

    #[test]
    fn reminders_quote_long_messages_cut_short() {
        let long = "x".repeat(QUOTE + 50);
        let r = reminder(&[long, "and this".into()]);
        assert!(r.contains(&format!("\"{}…\"", "x".repeat(QUOTE))));
        assert!(r.contains("and \"and this\""));
        assert!(r.starts_with("(From tab, not the person.)"));
    }

    #[tokio::test]
    async fn output_after_the_result_marks_the_session_working_again() {
        let store = Arc::new(Store::in_memory().unwrap());
        let (tx, _rx) = tokio::sync::broadcast::channel(64);
        let hub = AgentHub::new(
            store.clone(),
            tx,
            AgentConfig::default(),
            std::env::temp_dir(),
        );
        let s = store
            .session_create(None, AgentKind::ClaudeCode, "t", ".", Utc::now())
            .unwrap();
        let mut p = Parser::new(hub, s.id.clone());
        p.handle(
            &serde_json::from_str(r#"{"type":"result","session_id":"abc","is_error":false}"#)
                .unwrap(),
        );
        assert!(p.finished && !p.failed);
        // A background command finished and the agent picked back up.
        p.handle(&serde_json::from_str(r#"{"type":"assistant","message":{"id":"m3","content":[{"type":"text","text":"Build done"}]}}"#).unwrap());
        assert!(!p.finished);
        let s = store.session_get(&s.id).unwrap().unwrap();
        assert_eq!(s.status, SessionStatus::Working);
    }

    #[tokio::test]
    async fn a_foreground_ci_watch_shows_monitoring_until_it_returns() {
        let store = Arc::new(Store::in_memory().unwrap());
        let (tx, _rx) = tokio::sync::broadcast::channel(64);
        let hub = AgentHub::new(
            store.clone(),
            tx,
            AgentConfig::default(),
            std::env::temp_dir(),
        );
        let s = store
            .session_create(None, AgentKind::ClaudeCode, "t", ".", Utc::now())
            .unwrap();
        let mut p = Parser::new(hub.clone(), s.id.clone());
        hub.update(&s.id, Some(SessionStatus::Working), None);
        let status = || store.session_get(&s.id).unwrap().unwrap().status;
        p.handle(&json!({"type":"assistant","message":{"id":"m1","content":[
            {"type":"tool_use","id":"t1","name":"Bash","input":{"command":"gh pr checks 89 --watch"}}
        ]}}));
        assert_eq!(status(), SessionStatus::Monitoring);
        p.handle(&json!({"type":"user","message":{"content":[
            {"type":"tool_result","tool_use_id":"t1","content":"All checks passed"}
        ]}}));
        assert_eq!(status(), SessionStatus::Working);
    }

    #[test]
    fn messages_queue_and_send_now_interrupts_only_a_running_tool() {
        let msg = steer_line(Steer::Message("hi".into()), true, 1).unwrap();
        assert_eq!(
            steer_line(Steer::Reminder("hi".into()), true, 2).unwrap(),
            msg
        );
        assert_eq!(
            (msg["type"].as_str(), msg["message"]["content"].as_str()),
            (Some("user"), Some("hi"))
        );
        let stop = steer_line(Steer::Interrupt, true, 7).unwrap();
        assert_eq!(stop["request"]["subtype"], "interrupt");
        assert_eq!(stop["request_id"], "tab-interrupt-7");
        // Between tool calls the queued message is read anyway.
        assert_eq!(steer_line(Steer::Interrupt, false, 8), None);
    }

    #[tokio::test]
    async fn an_interrupt_to_steer_does_not_end_the_turn() {
        let store = Arc::new(Store::in_memory().unwrap());
        let (tx, _rx) = tokio::sync::broadcast::channel(64);
        let hub = AgentHub::new(
            store.clone(),
            tx,
            AgentConfig::default(),
            std::env::temp_dir(),
        );
        let s = store
            .session_create(None, AgentKind::ClaudeCode, "t", ".", Utc::now())
            .unwrap();
        hub.steering_open(&s.id);
        let mut p = Parser::new(hub.clone(), s.id.clone());
        let feed = |p: &mut Parser, line: &str| p.handle(&serde_json::from_str(line).unwrap());
        feed(
            &mut p,
            r#"{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"gh pr checks --watch"}}]}}"#,
        );
        assert!(p.open_tools.lock().unwrap().contains("t1"));
        // What Claude Code prints after an interrupt mid-tool.
        feed(
            &mut p,
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"The user doesn't want to proceed with this tool use.","is_error":true}]}}"#,
        );
        feed(
            &mut p,
            r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"[Request interrupted by user for tool use]"}]}}"#,
        );
        assert!(p.open_tools.lock().unwrap().is_empty());
        feed(
            &mut p,
            r#"{"type":"result","session_id":"abc","is_error":false,"stop_reason":"tool_use"}"#,
        );
        assert!(!p.finished, "the turn goes on with the steering message");
        assert!(hub.can_steer(&s.id), "steering stays open");
        let kinds: Vec<_> = store
            .messages(&s.id)
            .unwrap()
            .into_iter()
            .map(|m| m.kind)
            .collect();
        assert!(!kinds.contains(&"result".to_string()));
        // The steering message is answered, and that result ends the turn.
        feed(
            &mut p,
            r#"{"type":"assistant","message":{"id":"m2","content":[{"type":"text","text":"Skipping the wait."}]}}"#,
        );
        feed(
            &mut p,
            r#"{"type":"result","session_id":"abc","is_error":false,"stop_reason":"end_turn"}"#,
        );
        assert!(p.finished);
        assert!(!hub.can_steer(&s.id));
    }

    #[test]
    fn strict_chats_load_only_the_repos_settings() {
        assert_eq!(
            isolation_args(true, Trust::Person),
            ["--strict-mcp-config", "--setting-sources", "project,local"]
        );
        assert!(isolation_args(false, Trust::Person).is_empty());
    }

    #[test]
    fn review_chats_are_always_strict_and_load_no_settings() {
        for chosen in [true, false] {
            for trust in [Trust::Agent, Trust::Untrusted] {
                assert_eq!(
                    isolation_args(chosen, trust),
                    ["--strict-mcp-config", "--setting-sources", ""]
                );
            }
        }
    }

    #[test]
    fn strict_flags_anything_but_tab_in_the_init_line() {
        let clean = json!({
            "mcp_servers": [{ "name": "tab", "status": "connected" }],
            "plugins": [{ "name": "cc-plugin-telemetry", "source": "cc-plugin-telemetry@builtin" }],
        });
        assert!(unexpected(&clean, &[]).is_empty());
        // Remote servers tab added itself are tab's connections.
        let connected = json!({ "mcp_servers": [{ "name": "tab" }, { "name": "linear" }] });
        assert!(unexpected(&connected, &["linear".into()]).is_empty());
        assert_eq!(unexpected(&connected, &[]), ["MCP server \"linear\""]);
        let leaky = json!({
            "mcp_servers": [{ "name": "tab" }, { "name": "claude.ai Health", "status": "connected" }],
            "plugins": [{ "name": "cloudflare", "source": "cloudflare@synced" }],
        });
        assert_eq!(
            unexpected(&leaky, &[]),
            ["MCP server \"claude.ai Health\"", "plugin \"cloudflare\""]
        );
    }

    #[tokio::test]
    async fn a_strict_parser_records_what_leaked() {
        let store = Arc::new(Store::in_memory().unwrap());
        let (tx, _rx) = tokio::sync::broadcast::channel(8);
        let hub = AgentHub::new(
            store.clone(),
            tx,
            AgentConfig::default(),
            std::env::temp_dir(),
        );
        let s = store
            .session_create(None, AgentKind::ClaudeCode, "t", ".", Utc::now())
            .unwrap();
        let init =
            json!({ "type": "system", "subtype": "init", "mcp_servers": [{ "name": "health" }] });
        let mut loose = Parser::new(hub.clone(), s.id.clone());
        loose.handle(&init);
        assert!(loose.leaked.is_empty());
        let mut p = Parser::new(hub, s.id.clone());
        p.strict = true;
        p.handle(&init);
        assert_eq!(p.leaked, ["MCP server \"health\""]);
    }

    #[tokio::test]
    async fn the_init_line_saves_each_mcp_server_status_for_the_chat() {
        let store = Arc::new(Store::in_memory().unwrap());
        let (tx, _rx) = tokio::sync::broadcast::channel(8);
        let dir = std::env::temp_dir().join(format!("tab-agent-mcp-{}", std::process::id()));
        let hub = AgentHub::new(store.clone(), tx, AgentConfig::default(), dir.clone());
        let s = store
            .session_create(None, AgentKind::ClaudeCode, "t", ".", Utc::now())
            .unwrap();
        assert!(hub.agent_mcp(&s.id).is_empty());
        let init = json!({ "type": "system", "subtype": "init", "mcp_servers": [
            { "name": "tab", "status": "connected" },
            { "name": "linear", "status": "needs-auth" },
            { "name": "odd" },
        ] });
        Parser::new(hub.clone(), s.id.clone()).handle(&init);
        let got = hub.agent_mcp(&s.id);
        assert_eq!(
            got.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(),
            ["tab", "linear", "odd"]
        );
        assert_eq!(got[1].status.as_deref(), Some("needs-auth"));
        assert_eq!(got[2].status, None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn the_initialize_answer_saves_commands_with_what_they_take() {
        let store = Arc::new(Store::in_memory().unwrap());
        let (tx, _rx) = tokio::sync::broadcast::channel(8);
        let dir = std::env::temp_dir().join(format!("tab-slash-{}", uuid::Uuid::new_v4()));
        let cwd = dir.join("repo");
        std::fs::create_dir_all(cwd.join(".claude/commands/team")).unwrap();
        std::fs::write(cwd.join(".claude/commands/team/ship.md"), "Ship it").unwrap();
        let hub = AgentHub::new(store.clone(), tx, AgentConfig::default(), dir.clone());
        let s = store
            .session_create(None, AgentKind::ClaudeCode, "t", ".", Utc::now())
            .unwrap();
        let mut p = Parser::new(hub.clone(), s.id.clone());
        p.cwd = Some(cwd);
        p.handle(&json!({ "type": "control_response", "response": {
            "subtype": "success", "request_id": INIT_REQUEST, "response": {
                "commands": [
                    { "name": "code-review", "description": "Review", "argumentHint": "[low|high] [<pr#>]", "aliases": ["review"], "builtin": true },
                    { "name": "team:ship", "description": "Ship it", "argumentHint": "" },
                    { "name": "pdf", "description": "PDFs", "argumentHint": "" },
                    { "name": "mcp__docs__search", "description": "Search docs", "argumentHint": "<query>" },
                    { "name": "__remote-workflow", "description": "", "argumentHint": "", "builtin": true },
                ],
                "agents": [{ "name": "Explore", "description": "Search code" }],
            },
        }}));
        let cmds = hub.slash_commands(AgentKind::ClaudeCode);
        let kinds: Vec<_> = cmds.iter().map(|c| (c.name.as_str(), c.kind)).collect();
        assert_eq!(
            kinds,
            [
                ("code-review", super::super::SlashKind::Builtin),
                ("team:ship", super::super::SlashKind::Command),
                ("pdf", super::super::SlashKind::Skill),
                ("mcp__docs__search", super::super::SlashKind::Mcp),
            ]
        );
        assert_eq!(cmds[0].argument_hint, "[low|high] [<pr#>]");
        assert_eq!(cmds[0].aliases, ["review"]);
        assert_eq!(hub.subagents()[0].name, "Explore");

        // The init line adds only names the answer didn't have.
        p.handle(&json!({ "type": "system", "subtype": "init", "slash_commands": ["code-review", "review", "extra"] }));
        let names: Vec<_> = hub
            .slash_commands(AgentKind::ClaudeCode)
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert_eq!(
            names,
            [
                "code-review",
                "team:ship",
                "pdf",
                "mcp__docs__search",
                "extra"
            ]
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn slash_commands_saved_as_bare_names_still_load() {
        let store = Arc::new(Store::in_memory().unwrap());
        let (tx, _rx) = tokio::sync::broadcast::channel(8);
        let dir = std::env::temp_dir().join(format!("tab-slash-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("slash_commands.json"), r#"["compact","deploy"]"#).unwrap();
        let hub = AgentHub::new(store, tx, AgentConfig::default(), dir.clone());
        let names: Vec<_> = hub
            .slash_commands(AgentKind::ClaudeCode)
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert_eq!(names, ["compact", "deploy"]);
        let _ = std::fs::remove_dir_all(dir);
    }
}
