//! Connections to remote MCP servers (Linear, Notion, Sentry, any URL).
//!
//! The person signs in once in the browser; tab keeps the sign-in out of
//! `config.json` (see [`vault`]) and refreshes it. Agents never see it:
//! each chat reaches a server through tab's own MCP server at
//! `/mcp/<id>` (see `crate::mcp`), which adds the token, applies the
//! person's choice for each tool (allowed, ask, off) and forwards the rest.
//!
//! Because they come in through tab's `--mcp-config`, these servers work in
//! strict chats too, where the person's own MCP servers don't load.

pub mod import;
pub mod oauth;
pub mod stdio;
pub mod sync;
pub use crate::vault;

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::broadcast;

use crate::vault::Vault;
use oauth::{Client, Discovered, Tokens};

/// The MCP revision tab speaks to servers it lists tools from.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// One MCP server, as saved in `config.json` (no secrets).
///
/// Either remote (`url`) or a local command tab runs as its child
/// (`command`); agents reach both the same way, through `/mcp/<id>`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RemoteMcpConfig {
    pub name: String,
    /// The server's address. Empty for a local command.
    #[serde(default)]
    pub url: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Whether the server wants a sign-in (it answered 401).
    #[serde(default)]
    pub sign_in: bool,
    /// The catalog entry it came from, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog: Option<String>,
    /// The person's choice per tool. Tools not listed get [`default_policy`].
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tools: BTreeMap<String, ToolPolicy>,
    /// A local command tab starts and talks to over stdio.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<LocalCommand>,
    /// Names of headers sent with every request (an API key). The values
    /// are in the vault ([`Secrets`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub headers: Vec<String>,
    /// Where it was imported from, so importing again finds it
    /// (see [`import`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imported_from: Option<String>,
    /// Whether work from it comes into the queue, where tab has a sync
    /// recipe for it ([`sync`]). `None` means yes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue: Option<bool>,
    /// What the queue watches, for recipes that ask (Notion: a search).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_query: Option<String>,
}

/// A local MCP server: the program, its arguments, and the names of its
/// environment variables (their values are in the vault, [`Secrets`]).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct LocalCommand {
    pub command: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<String>,
    /// The folder it runs in (a project's servers run in the project).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

/// Header and environment values a server needs, kept in the vault next to
/// sign-ins (they're often API keys).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct Secrets {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
}

impl Secrets {
    pub fn is_empty(&self) -> bool {
        self.headers.is_empty() && self.env.is_empty()
    }
}

fn yes() -> bool {
    true
}

/// Where a server can run, for the "✓ This computer ✓ Cloud" marks.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CloudFit {
    /// Nothing ties it to this computer.
    pub portable: bool,
    /// Why it can only run here, in a few words.
    pub why_not: Option<String>,
    /// It sends a key, which would have to go to the hub (the person decides).
    pub needs_key: bool,
}

/// Whether a server could run in tab cloud: no `localhost`, `*.local` or
/// private addresses; no absolute paths or paths under the home folder; a
/// local command only when it's a package runner (`npx`, `uvx`, …) the
/// runner image has.
pub fn cloud_fit(c: &RemoteMcpConfig, home: Option<&std::path::Path>) -> CloudFit {
    let needs_key = !c.headers.is_empty() || c.command.as_ref().is_some_and(|l| !l.env.is_empty());
    let no = |why: &str| CloudFit {
        portable: false,
        why_not: Some(why.to_string()),
        needs_key,
    };
    if let Some(l) = &c.command {
        const RUNNERS: [&str; 6] = ["npx", "uvx", "bunx", "pnpx", "node", "python"];
        let program = std::path::Path::new(&l.command)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if local_path(&l.command, home) {
            return no("runs a program from this computer");
        }
        if !RUNNERS.contains(&program.as_str()) && !program.starts_with("python3") {
            return no(&format!("runs {program}, which tab cloud doesn't have"));
        }
        if l.args.iter().any(|a| local_path(a, home)) {
            return no("uses files on this computer");
        }
        if l.args.iter().any(|a| private_host_in(a)) {
            return no("talks to this computer or your network");
        }
    } else if private_host_in(&c.url) {
        return no("talks to this computer or your network");
    }
    CloudFit {
        portable: true,
        why_not: None,
        needs_key,
    }
}

/// An absolute path, a `~` path, or one under the home folder.
fn local_path(s: &str, home: Option<&std::path::Path>) -> bool {
    let s = s.split_once('=').map_or(s, |(_, v)| v);
    let bytes = s.as_bytes();
    let windows_abs = bytes.len() > 2
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/');
    s.starts_with('/')
        || s.starts_with('~')
        || s.starts_with("\\\\")
        || windows_abs
        || home.is_some_and(|h| {
            let h = h.to_string_lossy();
            !h.is_empty() && s.contains(h.as_ref())
        })
}

/// Whether `s` (a URL, or an argument holding one) names this computer or
/// a private network.
fn private_host_in(s: &str) -> bool {
    let Some(start) = s.find("://") else {
        return false;
    };
    let rest = &s[start + 3..];
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host_port = authority.rsplit('@').next().unwrap_or_default();
    let host = if let Some(h) = host_port.strip_prefix('[') {
        h.split(']').next().unwrap_or_default()
    } else {
        host_port.split(':').next().unwrap_or_default()
    }
    .to_ascii_lowercase();
    if host == "localhost" || host.ends_with(".localhost") || host.ends_with(".local") {
        return true;
    }
    match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(ip)) => {
            ip.is_loopback() || ip.is_private() || ip.is_link_local() || ip.is_unspecified()
        }
        Ok(std::net::IpAddr::V6(ip)) => {
            ip.is_loopback() || ip.is_unspecified() || (ip.segments()[0] & 0xfe00) == 0xfc00
        }
        Err(_) => false,
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolPolicy {
    /// Runs without asking.
    Allow,
    /// Asks the person in the chat each time.
    Ask,
    /// Agents don't see it.
    Off,
}

/// Read-only tools run; anything else asks first.
pub fn default_policy(tool: &Value) -> ToolPolicy {
    if tool["annotations"]["readOnlyHint"] == true {
        ToolPolicy::Allow
    } else {
        ToolPolicy::Ask
    }
}

/// A service tab knows the MCP server of.
#[derive(Debug, Clone, Serialize)]
pub struct CatalogEntry {
    pub id: &'static str,
    pub name: &'static str,
    pub url: &'static str,
    pub description: &'static str,
    /// Marketplace shelf, as for connectors: `code`, `work`, `communication`.
    pub category: &'static str,
    /// Brand-ish color for the letter mark.
    pub color: &'static str,
    /// tab has a sync recipe for it, so work from it fills the queue.
    pub fills_queue: bool,
    /// The kinds of work it brings, for the queue's labels and icons.
    pub kinds: Vec<crate::model::KindInfo>,
    /// What the queue watches, for recipes that ask (see
    /// [`RemoteMcpConfig::queue_query`]): the placeholder to show.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queue_query: Option<&'static str>,
}

/// Services whose servers let tab register itself (no app to set up).
pub fn catalog() -> Vec<CatalogEntry> {
    let mut all = vec![
        CatalogEntry {
            id: "linear",
            name: "Linear",
            url: "https://mcp.linear.app/mcp",
            description: "Agents can find, create and update issues, projects and comments.",
            category: "work",
            color: "#5e6ad2",
            fills_queue: false,
            kinds: Vec::new(),
            queue_query: None,
        },
        CatalogEntry {
            id: "notion",
            name: "Notion",
            url: "https://mcp.notion.com/mcp",
            description: "Agents can search, read and edit pages and databases.",
            category: "work",
            color: "#191919",
            fills_queue: false,
            kinds: Vec::new(),
            queue_query: None,
        },
        CatalogEntry {
            id: "atlassian",
            name: "Jira & Confluence",
            url: "https://mcp.atlassian.com/v1/mcp",
            description: "Agents can search, comment on and move tickets, and read pages.",
            category: "work",
            color: "#1868db",
            fills_queue: false,
            kinds: Vec::new(),
            queue_query: None,
        },
        CatalogEntry {
            id: "sentry",
            name: "Sentry",
            url: "https://mcp.sentry.dev/mcp",
            description: "Agents can look up issues, stack traces and events.",
            category: "code",
            color: "#362d59",
            fills_queue: false,
            kinds: Vec::new(),
            queue_query: None,
        },
        CatalogEntry {
            id: "vercel",
            name: "Vercel",
            url: "https://mcp.vercel.com",
            description: "Agents can read deployments, build logs and project settings.",
            category: "code",
            color: "#000000",
            fills_queue: false,
            kinds: Vec::new(),
            queue_query: None,
        },
        CatalogEntry {
            id: "stripe",
            name: "Stripe",
            url: "https://mcp.stripe.com",
            description: "Agents can look up customers, payments, invoices and disputes.",
            category: "work",
            color: "#635bff",
            fills_queue: false,
            kinds: Vec::new(),
            queue_query: None,
        },
    ];
    for e in &mut all {
        if let Some(r) = sync::recipe(e.id) {
            e.fills_queue = true;
            e.kinds = r.kinds();
            e.queue_query = r.query_hint();
        }
    }
    all
}

/// What tab keeps in the [`Vault`] for a server that wants a sign-in.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SignIn {
    pub discovered: Discovered,
    pub client: Client,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<Tokens>,
    pub signed_in_at: chrono::DateTime<Utc>,
}

/// One server's state for the Connections tab.
#[derive(Debug, Clone, Serialize)]
pub struct RemoteMcpInfo {
    pub id: String,
    #[serde(flatten)]
    pub config: RemoteMcpConfig,
    /// Signed in (or no sign-in needed): agents get its tools.
    pub ready: bool,
    pub signed_in_at: Option<chrono::DateTime<Utc>>,
    /// The last thing that went wrong (a refresh that failed, …).
    pub error: Option<String>,
    /// How many tools it had when tab last looked.
    pub tool_count: Option<usize>,
    /// Whether it could run in tab cloud.
    pub cloud: CloudFit,
    /// It has header or environment values in the vault.
    pub keys: bool,
    /// Where filling the queue from it stands. `None` when tab has no
    /// recipe for it (agent tools only).
    pub queue_sync: Option<QueueState>,
}

/// How filling the queue from one connection is going.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct QueueState {
    /// The person wants its work in the queue.
    pub on: bool,
    pub last_sync: Option<chrono::DateTime<Utc>>,
    /// What went wrong on the last look, if it did.
    pub error: Option<String>,
    /// How many open things it found last time.
    pub found: Option<usize>,
}

/// One tool as the detail page shows it.
#[derive(Debug, Clone, Serialize)]
pub struct ToolView {
    pub name: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub read_only: bool,
    pub policy: ToolPolicy,
    /// Whether `policy` is the person's choice rather than the default.
    pub chosen: bool,
}

/// What tab found at a URL before adding it.
#[derive(Debug, Clone, Serialize)]
pub struct Probe {
    pub url: String,
    pub name: Option<String>,
    pub sign_in: bool,
    /// A suggested id (the MCP server name agents see).
    pub id: String,
}

/// A sign-in finished (or failed) in the browser.
#[derive(Debug, Clone, Serialize)]
pub struct SignInEvent {
    pub id: String,
    pub ok: bool,
    pub error: Option<String>,
}

struct Pending {
    id: String,
    verifier: String,
    discovered: Discovered,
    client: Client,
    started: std::time::Instant,
}

/// Sign-ins started this long ago are dropped.
const PENDING_FOR: std::time::Duration = std::time::Duration::from_secs(15 * 60);

/// Whether `id` can name a connection: what agents see as the MCP server
/// name (`mcp__<id>__tool`), so short, lowercase, and not tab's own.
pub fn valid_id(id: &str) -> bool {
    (1..=32).contains(&id.len())
        && id != "tab"
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
        && id.chars().next().is_some_and(|c| c.is_ascii_lowercase())
}

/// An id for `name` that isn't taken.
pub fn suggest_id(name: &str, taken: &[String]) -> String {
    let mut base: String = name
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    base.truncate(24);
    let base = base.trim_end_matches('-').to_string();
    let base = if valid_id(&base) {
        base
    } else {
        format!(
            "mcp-{}",
            base.trim_start_matches(|c: char| !c.is_ascii_lowercase())
        )
        .trim_end_matches('-')
        .to_string()
    };
    let base = if valid_id(&base) { base } else { "mcp".into() };
    if !taken.contains(&base) {
        return base;
    }
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|c| !taken.contains(c))
        .unwrap()
}

/// Remote servers are HTTPS, except on this computer.
pub fn check_url(url: &str) -> anyhow::Result<reqwest::Url> {
    let u = reqwest::Url::parse(url.trim()).map_err(|_| {
        anyhow::anyhow!("That isn't a URL. It should look like https://example.com/mcp")
    })?;
    let local = matches!(u.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    anyhow::ensure!(
        u.scheme() == "https" || (u.scheme() == "http" && local),
        "MCP servers need an https:// address"
    );
    Ok(u)
}

/// JSON-RPC messages in a response body: plain JSON, or the `data:` lines
/// of a server-sent event stream.
pub fn messages(content_type: &str, body: &str) -> Vec<Value> {
    if content_type.starts_with("text/event-stream") {
        sse_data(body)
            .iter()
            .filter_map(|d| serde_json::from_str(d).ok())
            .collect()
    } else {
        match serde_json::from_str::<Value>(body) {
            Ok(Value::Array(a)) => a,
            Ok(v) => vec![v],
            Err(_) => Vec::new(),
        }
    }
}

/// The data of each event in an SSE body.
fn sse_data(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur: Option<String> = None;
    for line in body.lines() {
        if line.is_empty() {
            out.extend(cur.take());
        } else if let Some(d) = line.strip_prefix("data:") {
            let d = d.strip_prefix(' ').unwrap_or(d);
            match &mut cur {
                Some(c) => {
                    c.push('\n');
                    c.push_str(d);
                }
                None => cur = Some(d.to_string()),
            }
        }
    }
    out.extend(cur);
    out
}

/// Rewrites every JSON-RPC message in a body with `f`, keeping the body's
/// format (JSON or SSE).
pub fn rewrite(content_type: &str, body: &str, f: impl Fn(&mut Value)) -> String {
    if content_type.starts_with("text/event-stream") {
        let mut out = String::new();
        for line in body.split_inclusive('\n') {
            let (content, end) = match line.strip_suffix('\n') {
                Some(c) => (c.strip_suffix('\r').unwrap_or(c), "\n"),
                None => (line, ""),
            };
            if let Some(d) = content.strip_prefix("data:")
                && let Ok(mut v) = serde_json::from_str::<Value>(d.trim_start())
            {
                f(&mut v);
                out.push_str("data: ");
                out.push_str(&v.to_string());
                out.push_str(end);
            } else {
                out.push_str(line);
            }
        }
        out
    } else {
        match serde_json::from_str::<Value>(body) {
            Ok(Value::Array(mut a)) => {
                a.iter_mut().for_each(&f);
                Value::Array(a).to_string()
            }
            Ok(mut v) => {
                f(&mut v);
                v.to_string()
            }
            Err(_) => body.to_string(),
        }
    }
}

pub struct RemoteMcp {
    http: reqwest::Client,
    vault: Vault,
    servers: RwLock<BTreeMap<String, RemoteMcpConfig>>,
    /// `http://127.0.0.1:<port>` of tab's MCP server, for the OAuth redirect.
    loopback: RwLock<Option<String>>,
    pending: Mutex<HashMap<String, Pending>>,
    /// Each server's tools as it last listed them.
    tools: Mutex<HashMap<String, Vec<Value>>>,
    errors: Mutex<HashMap<String, String>>,
    refreshing: tokio::sync::Mutex<()>,
    events: broadcast::Sender<SignInEvent>,
    /// Local servers tab runs.
    local: stdio::Children,
    /// How each connection's queue sync went last ([`sync`]).
    synced: Mutex<HashMap<String, QueueState>>,
}

impl RemoteMcp {
    /// `dir` holds the sign-ins on platforms that keep them in files.
    pub fn new(dir: PathBuf) -> Arc<Self> {
        let http = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .user_agent(concat!("tab/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("reqwest client");
        Arc::new(Self {
            http,
            vault: Vault::new(dir),
            servers: Default::default(),
            loopback: Default::default(),
            pending: Default::default(),
            tools: Default::default(),
            errors: Default::default(),
            refreshing: Default::default(),
            events: broadcast::channel(16).0,
            local: Default::default(),
            synced: Default::default(),
        })
    }

    /// tab's MCP server (`http://127.0.0.1:port`), where sign-ins come back.
    pub fn loopback(&self) -> Option<String> {
        self.loopback.read().unwrap().clone()
    }

    pub fn set_loopback(&self, base: &str) {
        *self.loopback.write().unwrap() = Some(base.trim_end_matches('/').to_string());
    }

    pub fn set_servers(&self, servers: BTreeMap<String, RemoteMcpConfig>) {
        // Local servers that were removed or turned off stop; one whose
        // command changed restarts on its next request.
        self.local.retain(|id| {
            servers
                .get(id)
                .is_some_and(|c| c.enabled && c.command.is_some())
        });
        *self.servers.write().unwrap() = servers;
    }

    /// Header and environment values for `id`.
    pub fn secrets(&self, id: &str) -> Secrets {
        self.vault
            .get(&secrets_key(id))
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn set_secrets(&self, id: &str, s: &Secrets) -> anyhow::Result<()> {
        if s.is_empty() {
            self.vault.remove(&secrets_key(id));
            return Ok(());
        }
        self.vault.set(&secrets_key(id), &serde_json::to_string(s)?)
    }

    /// Forgets everything kept for `id` (sign-in and keys) and stops it.
    pub fn forget(&self, id: &str) {
        self.sign_out(id);
        self.vault.remove(&secrets_key(id));
        self.local.stop(id);
    }

    /// What a local server runs with, if `id` is one.
    fn local_spec(&self, id: &str) -> Option<stdio::Spec> {
        let command = self.get(id)?.command?;
        let mut env = self.secrets(id).env;
        env.retain(|k, _| command.env.contains(k));
        Some(stdio::Spec { command, env })
    }

    /// Whether `id` is a local command rather than a URL.
    pub fn is_local(&self, id: &str) -> bool {
        self.get(id).is_some_and(|c| c.command.is_some())
    }

    /// One message from a chat to a local server (see [`stdio`]).
    pub async fn forward_local(&self, id: &str, msg: &Value) -> anyhow::Result<Option<Value>> {
        let spec = self
            .local_spec(id)
            .ok_or_else(|| anyhow::anyhow!("{id} isn't a local server"))?;
        let r = self.local.forward(id, &spec, msg).await;
        self.set_error(id, r.as_ref().err().map(|e| e.to_string()));
        r
    }

    pub fn subscribe(&self) -> broadcast::Receiver<SignInEvent> {
        self.events.subscribe()
    }

    pub fn get(&self, id: &str) -> Option<RemoteMcpConfig> {
        self.servers.read().unwrap().get(id).cloned()
    }

    fn sign_in_record(&self, id: &str) -> Option<SignIn> {
        serde_json::from_str(&self.vault.get(id)?).ok()
    }

    fn save_sign_in(&self, id: &str, s: &SignIn) -> anyhow::Result<()> {
        self.vault.set(id, &serde_json::to_string(s)?)
    }

    fn ready(&self, id: &str, c: &RemoteMcpConfig) -> bool {
        c.enabled
            && (c.command.is_some()
                || !c.sign_in
                || self.sign_in_record(id).is_some_and(|s| s.tokens.is_some()))
    }

    /// Whether `id` is on and signed in where that's needed.
    pub fn is_ready(&self, id: &str) -> bool {
        self.get(id).is_some_and(|c| self.ready(id, &c))
    }

    /// Records how a queue sync went (see [`sync`]).
    pub(crate) fn set_synced(&self, id: &str, f: impl FnOnce(&mut QueueState)) {
        let mut all = self.synced.lock().unwrap();
        f(all.entry(id.to_string()).or_default());
    }

    /// Calls one tool as tab itself (not for an agent), and returns its
    /// result. A result the server marks as an error comes back as `Err`.
    pub async fn call_tool(&self, id: &str, tool: &str, args: Value) -> anyhow::Result<Value> {
        let result = self
            .rpc(id, "tools/call", json!({ "name": tool, "arguments": args }))
            .await?;
        if result["isError"] == true {
            let text = sync::text_of(&result);
            anyhow::bail!(
                "{tool}: {}",
                if text.is_empty() {
                    "the server returned an error"
                } else {
                    text.trim()
                }
            );
        }
        Ok(result)
    }

    /// The tools `id` has, asking it when tab hasn't looked yet.
    pub(crate) async fn tool_names(&self, id: &str) -> anyhow::Result<Vec<String>> {
        let names = |all: &HashMap<String, Vec<Value>>| {
            all.get(id).map(|t| {
                t.iter()
                    .filter_map(|t| t["name"].as_str().map(str::to_string))
                    .collect::<Vec<_>>()
            })
        };
        if let Some(n) = names(&self.tools.lock().unwrap()).filter(|n| !n.is_empty()) {
            return Ok(n);
        }
        self.list_tools(id).await?;
        Ok(names(&self.tools.lock().unwrap()).unwrap_or_default())
    }

    /// Servers agents get: on, and signed in where that's needed.
    pub fn usable(&self) -> Vec<String> {
        let servers = self.servers.read().unwrap().clone();
        servers
            .iter()
            .filter(|(id, c)| self.ready(id, c))
            .map(|(id, _)| id.clone())
            .collect()
    }

    pub fn infos(&self) -> Vec<RemoteMcpInfo> {
        let servers = self.servers.read().unwrap().clone();
        let home = crate::sandbox::home();
        servers
            .into_iter()
            .map(|(id, config)| {
                let record = self.sign_in_record(&id);
                RemoteMcpInfo {
                    cloud: cloud_fit(&config, home.as_deref()),
                    keys: !self.secrets(&id).is_empty(),
                    ready: self.ready(&id, &config),
                    signed_in_at: record
                        .filter(|r| r.tokens.is_some())
                        .map(|r| r.signed_in_at),
                    error: self.errors.lock().unwrap().get(&id).cloned(),
                    tool_count: self.tools.lock().unwrap().get(&id).map(Vec::len),
                    queue_sync: sync::recipe_for(&id, &config).map(|_| {
                        let mut q = self
                            .synced
                            .lock()
                            .unwrap()
                            .get(&id)
                            .cloned()
                            .unwrap_or_default();
                        q.on = config.queue != Some(false);
                        q
                    }),
                    id,
                    config,
                }
            })
            .collect()
    }

    fn set_error(&self, id: &str, e: Option<String>) {
        let mut errors = self.errors.lock().unwrap();
        match e {
            Some(e) => errors.insert(id.to_string(), e),
            None => errors.remove(id),
        };
    }

    /// An `initialize` request without a token: does it want a sign-in, and
    /// what does it call itself?
    pub async fn probe(&self, url: &str, taken: &[String]) -> anyhow::Result<Probe> {
        let u = check_url(url)?;
        let r = self
            .http
            .post(u.as_str())
            .timeout(std::time::Duration::from_secs(20))
            .header("accept", "application/json, text/event-stream")
            .json(&initialize(0))
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Couldn't reach {}: {e}", u.host_str().unwrap_or("it")))?;
        let status = r.status();
        let (sign_in, name) = if status == reqwest::StatusCode::UNAUTHORIZED {
            let challenge = r
                .headers()
                .get("www-authenticate")
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            // The resource's own name, when its metadata has one.
            let mut name = None;
            let mut urls: Vec<String> = challenge
                .as_deref()
                .and_then(|c| oauth::challenge_param(c, "resource_metadata"))
                .into_iter()
                .collect();
            urls.extend(oauth::resource_metadata_urls(u.as_str()));
            for m in urls {
                if let Ok(r) = self.http.get(&m).send().await
                    && let Ok(v) = r.json::<Value>().await
                    && v["authorization_servers"].is_array()
                {
                    name = v["resource_name"].as_str().map(str::to_string);
                    break;
                }
            }
            (true, name)
        } else if status.is_success() {
            let ct = content_type(r.headers());
            let body = r.text().await.unwrap_or_default();
            let name = messages(&ct, &body).into_iter().find_map(|m| {
                m["result"]["serverInfo"]["name"]
                    .as_str()
                    .map(str::to_string)
            });
            anyhow::ensure!(
                name.is_some(),
                "{} answered, but not like an MCP server",
                u.host_str().unwrap_or("It")
            );
            (false, name)
        } else {
            anyhow::bail!(
                "{} answered {status}. Check the address.",
                u.host_str().unwrap_or("The server")
            );
        };
        let host_name = u
            .host_str()
            .unwrap_or("mcp")
            .trim_start_matches("mcp.")
            .split('.')
            .next()
            .unwrap_or("mcp")
            .to_string();
        let id = suggest_id(
            catalog()
                .iter()
                .find(|c| {
                    c.url == u.as_str()
                        || c.url.trim_end_matches('/') == u.as_str().trim_end_matches('/')
                })
                .map(|c| c.id)
                .unwrap_or(&host_name),
            taken,
        );
        Ok(Probe {
            url: u.to_string(),
            name,
            sign_in,
            id,
        })
    }

    /// Starts signing in to `id`. Returns the page to open in the browser;
    /// the browser comes back to `/oauth/callback` on tab's MCP server.
    pub async fn begin_sign_in(&self, id: &str) -> anyhow::Result<String> {
        let cfg = self
            .get(id)
            .ok_or_else(|| anyhow::anyhow!("no connection {id}"))?;
        let base = self.loopback.read().unwrap().clone().ok_or_else(|| {
            anyhow::anyhow!(
                "tab's MCP server isn't running, so the sign-in has nowhere to come back to"
            )
        })?;
        let redirect = format!("{base}/oauth/callback");
        let r = self
            .http
            .post(&cfg.url)
            .timeout(std::time::Duration::from_secs(20))
            .header("accept", "application/json, text/event-stream")
            .json(&initialize(0))
            .send()
            .await?;
        let challenge = r
            .headers()
            .get("www-authenticate")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let discovered = oauth::discover(&self.http, &cfg.url, challenge.as_deref()).await?;
        // Registered before, with the same server and address: reuse it.
        let client = match self.sign_in_record(id) {
            Some(s)
                if s.discovered.issuer == discovered.issuer
                    && s.client.redirect_uri == redirect =>
            {
                s.client
            }
            _ => oauth::register(&self.http, &discovered, &redirect).await?,
        };
        let (verifier, challenge) = oauth::pkce();
        let state = uuid::Uuid::new_v4().simple().to_string();
        let url = oauth::authorize_url(&discovered, &client, &state, &challenge)?;
        let mut pending = self.pending.lock().unwrap();
        pending.retain(|_, p| p.started.elapsed() < PENDING_FOR && p.id != id);
        pending.insert(
            state,
            Pending {
                id: id.to_string(),
                verifier,
                discovered,
                client,
                started: std::time::Instant::now(),
            },
        );
        Ok(url)
    }

    /// The browser came back. Returns the connection's name for the page
    /// the browser shows.
    pub async fn finish_sign_in(
        &self,
        state: &str,
        code: Option<&str>,
        error: Option<&str>,
    ) -> anyhow::Result<String> {
        let p = self
            .pending
            .lock()
            .unwrap()
            .remove(state)
            .filter(|p| p.started.elapsed() < PENDING_FOR)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "This sign-in link expired or was already used. Start again in tab."
                )
            })?;
        let name = self.get(&p.id).map(|c| c.name).unwrap_or(p.id.clone());
        let result = async {
            if let Some(e) = error {
                anyhow::bail!("{name} said: {e}");
            }
            let code = code.ok_or_else(|| anyhow::anyhow!("{name} sent no code back"))?;
            let tokens = oauth::exchange(
                &self.http,
                &p.discovered,
                &p.client,
                code,
                &p.verifier,
                Utc::now(),
            )
            .await
            .map_err(|e| anyhow::anyhow!("{name} didn't accept the sign-in: {e}"))?;
            self.save_sign_in(
                &p.id,
                &SignIn {
                    discovered: p.discovered.clone(),
                    client: p.client.clone(),
                    tokens: Some(tokens),
                    signed_in_at: Utc::now(),
                },
            )?;
            Ok(())
        }
        .await;
        self.tools.lock().unwrap().remove(&p.id);
        self.set_error(&p.id, result.as_ref().err().map(|e| e.to_string()));
        let _ = self.events.send(SignInEvent {
            id: p.id.clone(),
            ok: result.is_ok(),
            error: result.as_ref().err().map(|e| e.to_string()),
        });
        result.map(|_| name)
    }

    /// Forgets the sign-in (the connection stays, signed out).
    pub fn sign_out(&self, id: &str) {
        self.vault.remove(id);
        self.tools.lock().unwrap().remove(id);
        self.set_error(id, None);
    }

    /// The access token to send, refreshed when it's stale or `force`.
    /// `None` for servers that need no sign-in.
    pub async fn token(&self, id: &str, force: bool) -> anyhow::Result<Option<String>> {
        let cfg = self
            .get(id)
            .ok_or_else(|| anyhow::anyhow!("no connection {id}"))?;
        if !cfg.sign_in {
            return Ok(None);
        }
        let current = |s: &Option<SignIn>| s.as_ref().and_then(|s| s.tokens.clone());
        let record = self.sign_in_record(id);
        let Some(tokens) = current(&record) else {
            anyhow::bail!("{} needs you to sign in again", cfg.name);
        };
        // A sign-in is for the server it was made with: if the connection's
        // address changed since, its token doesn't go to the new one.
        if let Some(r) = &record
            && !oauth::resource_matches(&r.discovered.resource, &cfg.url)
        {
            anyhow::bail!(
                "{}'s address changed since you signed in; sign in again",
                cfg.name
            );
        }
        if !force && !tokens.stale(Utc::now()) {
            return Ok(Some(tokens.access_token));
        }
        let _one_at_a_time = self.refreshing.lock().await;
        // Another request may have refreshed it while this one waited.
        let mut record = self.sign_in_record(id).unwrap();
        let latest = record.tokens.clone().unwrap_or(tokens.clone());
        if latest.access_token != tokens.access_token && !latest.stale(Utc::now()) {
            return Ok(Some(latest.access_token));
        }
        match oauth::refresh(
            &self.http,
            &record.discovered,
            &record.client,
            &latest,
            Utc::now(),
        )
        .await
        {
            Ok(t) => {
                let access = t.access_token.clone();
                record.tokens = Some(t);
                self.save_sign_in(id, &record)?;
                self.set_error(id, None);
                Ok(Some(access))
            }
            Err(e) => {
                tracing::warn!(id, error = %e, "MCP sign-in refresh failed");
                // A refresh token that's no longer good won't get better.
                record.tokens = None;
                let _ = self.save_sign_in(id, &record);
                let msg = format!("{} needs you to sign in again", cfg.name);
                self.set_error(id, Some(msg.clone()));
                anyhow::bail!(msg)
            }
        }
    }

    /// Sends one request to the server with its sign-in, refreshing and
    /// retrying once if the server says the token is no good.
    pub async fn send(
        &self,
        id: &str,
        method: reqwest::Method,
        headers: &[(String, String)],
        body: Option<bytes::Bytes>,
    ) -> anyhow::Result<reqwest::Response> {
        let cfg = self
            .get(id)
            .ok_or_else(|| anyhow::anyhow!("no connection {id}"))?;
        let mut force = false;
        loop {
            let token = self.token(id, force).await?;
            let mut req = self.http.request(method.clone(), &cfg.url);
            for (k, v) in headers {
                req = req.header(k, v);
            }
            if !cfg.headers.is_empty() {
                for (k, v) in self.secrets(id).headers {
                    if cfg.headers.contains(&k) {
                        req = req.header(k, v);
                    }
                }
            }
            if let Some(t) = &token {
                req = req.bearer_auth(t);
            }
            if let Some(b) = &body {
                req = req.body(b.clone());
            }
            let r = req.send().await?;
            if r.status() == reqwest::StatusCode::UNAUTHORIZED && cfg.sign_in && !force {
                force = true;
                continue;
            }
            return Ok(r);
        }
    }

    /// The person's choice for a tool, or the default for its kind.
    pub fn policy(&self, id: &str, tool: &str) -> ToolPolicy {
        if let Some(p) = self.get(id).and_then(|c| c.tools.get(tool).copied()) {
            return p;
        }
        self.tools
            .lock()
            .unwrap()
            .get(id)
            .and_then(|t| t.iter().find(|t| t["name"] == tool))
            .map(default_policy)
            // Not seen yet: ask, rather than guess it only reads.
            .unwrap_or(ToolPolicy::Ask)
    }

    /// Remembers a server's tools from a `tools/list` result that passed
    /// through, so policies know which tools only read.
    pub fn saw_tools(&self, id: &str, tools: &[Value], first_page: bool) {
        let mut all = self.tools.lock().unwrap();
        let list = all.entry(id.to_string()).or_default();
        if first_page {
            list.clear();
        }
        for t in tools {
            if !list.iter().any(|x| x["name"] == t["name"]) {
                list.push(t.clone());
            }
        }
    }

    /// One JSON-RPC request in a fresh MCP session (initialize first).
    async fn rpc(&self, id: &str, method: &str, params: Value) -> anyhow::Result<Value> {
        if let Some(spec) = self.local_spec(id) {
            return self.local.rpc(id, &spec, method, params).await;
        }
        let base = vec![
            ("content-type".to_string(), "application/json".to_string()),
            (
                "accept".to_string(),
                "application/json, text/event-stream".to_string(),
            ),
        ];
        let r = self
            .send(
                id,
                reqwest::Method::POST,
                &base,
                Some(initialize(1).to_string().into()),
            )
            .await?;
        anyhow::ensure!(
            r.status().is_success(),
            "the server answered {}",
            r.status()
        );
        let session = r
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let ct = content_type(r.headers());
        let init = messages(&ct, &r.text().await?);
        let version = init
            .iter()
            .find_map(|m| m["result"]["protocolVersion"].as_str())
            .unwrap_or(PROTOCOL_VERSION)
            .to_string();
        let mut headers = base.clone();
        headers.push(("mcp-protocol-version".into(), version));
        if let Some(s) = &session {
            headers.push(("mcp-session-id".into(), s.clone()));
        }
        let note = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
        let _ = self
            .send(
                id,
                reqwest::Method::POST,
                &headers,
                Some(note.to_string().into()),
            )
            .await;
        let req = json!({ "jsonrpc": "2.0", "id": 2, "method": method, "params": params });
        let r = self
            .send(
                id,
                reqwest::Method::POST,
                &headers,
                Some(req.to_string().into()),
            )
            .await?;
        anyhow::ensure!(
            r.status().is_success(),
            "the server answered {}",
            r.status()
        );
        let ct = content_type(r.headers());
        let reply = messages(&ct, &r.text().await?)
            .into_iter()
            .find(|m| m["id"] == 2)
            .ok_or_else(|| anyhow::anyhow!("the server sent no answer"))?;
        if session.is_some() {
            let mut h = headers.clone();
            h.retain(|(k, _)| k != "content-type");
            let _ = self.send(id, reqwest::Method::DELETE, &h, None).await;
        }
        if let Some(e) = reply.get("error") {
            anyhow::bail!(
                "{}",
                e["message"]
                    .as_str()
                    .unwrap_or("the server returned an error")
            );
        }
        Ok(reply["result"].clone())
    }

    /// Asks the server for its tools (every page), and remembers them.
    pub async fn list_tools(&self, id: &str) -> anyhow::Result<Vec<ToolView>> {
        let mut tools: Vec<Value> = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..20 {
            let params = match &cursor {
                Some(c) => json!({ "cursor": c }),
                None => json!({}),
            };
            let result = self.rpc(id, "tools/list", params).await?;
            tools.extend(result["tools"].as_array().cloned().unwrap_or_default());
            cursor = result["nextCursor"].as_str().map(str::to_string);
            if cursor.is_none() {
                break;
            }
        }
        self.saw_tools(id, &tools, true);
        self.set_error(id, None);
        Ok(self.tool_views(id))
    }

    /// The tools tab last saw, with the person's choices.
    pub fn tool_views(&self, id: &str) -> Vec<ToolView> {
        let chosen = self.get(id).map(|c| c.tools).unwrap_or_default();
        let tools = self
            .tools
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .unwrap_or_default();
        tools
            .iter()
            .map(|t| {
                let name = t["name"].as_str().unwrap_or_default().to_string();
                let choice = chosen.get(&name).copied();
                ToolView {
                    title: t["title"]
                        .as_str()
                        .or_else(|| t["annotations"]["title"].as_str())
                        .map(str::to_string),
                    description: t["description"].as_str().map(str::to_string),
                    read_only: t["annotations"]["readOnlyHint"] == true,
                    policy: choice.unwrap_or_else(|| default_policy(t)),
                    chosen: choice.is_some(),
                    name,
                }
            })
            .collect()
    }
}

/// The vault entry for a server's keys (ids can't contain dots, so it can't
/// clash with a sign-in).
fn secrets_key(id: &str) -> String {
    format!("{id}.keys")
}

/// An `initialize` request from tab.
fn initialize(id: u64) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "initialize",
        "params": {
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": { "name": "tab", "version": env!("CARGO_PKG_VERSION") }
        }
    })
}

pub fn content_type(h: &reqwest::header::HeaderMap) -> String {
    h.get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/json")
        .to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_server_names_agents_can_use() {
        assert!(valid_id("linear"));
        assert!(valid_id("acme-api_2"));
        assert!(!valid_id("tab"));
        assert!(!valid_id("Linear"));
        assert!(!valid_id("2fa"));
        assert!(!valid_id(""));
        assert!(!valid_id("has space"));
        assert_eq!(suggest_id("Jira & Confluence", &[]), "jira-confluence");
        assert_eq!(suggest_id("Linear", &["linear".into()]), "linear-2");
        assert_eq!(suggest_id("tab", &[]), "mcp-tab");
        assert_eq!(suggest_id("123", &[]), "mcp");
        assert!(catalog().iter().all(|c| valid_id(c.id)));
    }

    #[test]
    fn only_local_servers_may_skip_https() {
        assert!(check_url("https://mcp.linear.app/mcp").is_ok());
        assert!(check_url("http://localhost:3000/mcp").is_ok());
        assert!(check_url("http://127.0.0.1:3000/mcp").is_ok());
        assert!(check_url("http://mcp.example.com/mcp").is_err());
        assert!(check_url("mcp.example.com").is_err());
    }

    #[test]
    fn read_only_tools_run_and_others_ask() {
        assert_eq!(
            default_policy(&json!({ "name": "list", "annotations": { "readOnlyHint": true } })),
            ToolPolicy::Allow
        );
        assert_eq!(
            default_policy(&json!({ "name": "create" })),
            ToolPolicy::Ask
        );
        assert_eq!(
            default_policy(&json!({ "name": "d", "annotations": { "readOnlyHint": false } })),
            ToolPolicy::Ask
        );
    }

    #[test]
    fn reads_json_and_sse_bodies() {
        let sse = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n\nid: 2\ndata: {\"jsonrpc\":\"2.0\",\ndata: \"id\":2}\n\n";
        let m = messages("text/event-stream", sse);
        assert_eq!(m.len(), 2);
        assert_eq!(m[1]["id"], 2);
        let m = messages("application/json", r#"[{"id":1},{"id":2}]"#);
        assert_eq!(m.len(), 2);
        assert!(messages("application/json", "nope").is_empty());
    }

    #[test]
    fn rewrites_keep_the_body_format() {
        let drop_b = |v: &mut Value| {
            if let Some(t) = v["result"]["tools"].as_array_mut() {
                t.retain(|t| t["name"] != "b");
            }
        };
        let sse = "event: message\r\ndata: {\"id\":1,\"result\":{\"tools\":[{\"name\":\"a\"},{\"name\":\"b\"}]}}\r\n\r\n";
        let out = rewrite("text/event-stream", sse, drop_b);
        assert!(out.starts_with("event: message\r\ndata: {"));
        assert!(out.ends_with("\n\r\n"));
        let m = messages("text/event-stream", &out);
        assert_eq!(m[0]["result"]["tools"].as_array().unwrap().len(), 1);
        let out = rewrite(
            "application/json",
            r#"{"id":1,"result":{"tools":[{"name":"a"},{"name":"b"}]}}"#,
            drop_b,
        );
        assert_eq!(out, r#"{"id":1,"result":{"tools":[{"name":"a"}]}}"#);
    }

    fn manager() -> Arc<RemoteMcp> {
        RemoteMcp::new(std::env::temp_dir().join(format!("tab-remote-{}", uuid::Uuid::new_v4())))
    }

    #[test]
    fn choices_beat_defaults_and_unknown_tools_ask() {
        let m = manager();
        let mut tools = BTreeMap::new();
        tools.insert("create_issue".into(), ToolPolicy::Allow);
        m.set_servers(BTreeMap::from([(
            "linear".into(),
            RemoteMcpConfig {
                name: "Linear".into(),
                url: "https://mcp.linear.app/mcp".into(),
                enabled: true,
                sign_in: false,
                catalog: Some("linear".into()),
                tools,
                ..Default::default()
            },
        )]));
        assert_eq!(m.policy("linear", "list_issues"), ToolPolicy::Ask);
        m.saw_tools(
            "linear",
            &[
                json!({ "name": "list_issues", "annotations": { "readOnlyHint": true } }),
                json!({ "name": "create_issue" }),
                json!({ "name": "delete_issue" }),
            ],
            true,
        );
        assert_eq!(m.policy("linear", "list_issues"), ToolPolicy::Allow);
        assert_eq!(m.policy("linear", "create_issue"), ToolPolicy::Allow);
        assert_eq!(m.policy("linear", "delete_issue"), ToolPolicy::Ask);
        let views = m.tool_views("linear");
        assert_eq!(views.len(), 3);
        assert!(views[0].read_only && !views[0].chosen);
        assert!(views[1].chosen);
        // No sign-in needed, so it's ready for agents.
        assert_eq!(m.usable(), ["linear"]);
    }

    #[test]
    fn servers_that_want_a_sign_in_wait_for_one() {
        let m = manager();
        m.set_servers(BTreeMap::from([(
            "notion".into(),
            RemoteMcpConfig {
                name: "Notion".into(),
                url: "https://mcp.notion.com/mcp".into(),
                enabled: true,
                sign_in: true,
                catalog: None,
                tools: Default::default(),
                ..Default::default()
            },
        )]));
        assert!(m.usable().is_empty());
        let info = &m.infos()[0];
        assert!(!info.ready);
        assert_eq!(info.signed_in_at, None);
    }
}
