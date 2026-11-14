//! Bringing the person's Claude Code MCP servers into tab
//! (docs/connect-via-mcp.md §4).
//!
//! Strict chats and cloud runs only see tab's connections, so the servers
//! someone set up in Claude Code are invisible there. This reads them and
//! offers each as a tab connection:
//!
//! - `~/.claude.json`: user servers (`mcpServers`) and each project's own
//!   (`projects.<path>.mcpServers`).
//! - `.mcp.json` in those projects and in the repos tab knows about.
//! - claude.ai connectors, by name only (tab can't read their settings):
//!   the ones that match a catalog entry become that connection.
//!
//! Importing copies. Nothing in Claude Code changes, and its OAuth tokens
//! stay its own: servers that sign in are signed in to again in tab.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

use super::{CloudFit, LocalCommand, RemoteMcpConfig, Secrets, catalog, cloud_fit, suggest_id};

/// One server found in Claude Code, as the import dialog shows it. Keys
/// and environment values never leave tab: the dialog gets their names.
#[derive(Debug, Clone, Serialize)]
pub struct Candidate {
    /// Stable across scans: where it came from and its name.
    pub key: String,
    /// Its name in Claude Code.
    pub name: String,
    /// `user`, `project` (Claude Code's own per-project list), `repo` (a
    /// `.mcp.json`) or `claude_ai`.
    pub source: &'static str,
    /// The project folder, for `project` and `repo`.
    pub path: Option<String>,
    /// `remote` (a URL, maybe with a sign-in), `remote_keys` (a URL with
    /// headers), `local` (a command) or `claude_ai`.
    pub kind: &'static str,
    pub url: Option<String>,
    /// The command line, for local servers.
    pub command: Option<String>,
    pub header_names: Vec<String>,
    pub env_names: Vec<String>,
    pub cloud: CloudFit,
    /// The catalog entry it is.
    pub catalog: Option<String>,
    pub status: Status,
    /// The tab connection it matches, if any.
    pub existing: Option<String>,
    /// The id it gets in tab (agents see `mcp__<id>__…`).
    pub id: String,
    /// Something to know before importing (a variable that isn't set, why
    /// it can't be imported).
    pub note: Option<String>,
    #[serde(skip)]
    pub config: Option<RemoteMcpConfig>,
    #[serde(skip)]
    pub secrets: Secrets,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Not in tab yet.
    New,
    /// Imported before, and changed in Claude Code since.
    Changed,
    /// Already a tab connection.
    Imported,
    /// tab can't bring it over.
    Unsupported,
}

impl Candidate {
    /// Whether the banner should count it.
    pub fn actionable(&self) -> bool {
        matches!(self.status, Status::New | Status::Changed)
    }
}

/// What Claude Code has, read from disk.
#[derive(Debug, Default)]
pub struct Sources {
    /// `~/.claude.json`.
    pub claude_json: Option<Value>,
    /// Each project's `.mcp.json`, by folder.
    pub mcp_json: Vec<(PathBuf, Value)>,
    /// claude.ai connector names tab saw in a chat (`claude.ai Linear`).
    pub claude_ai: Vec<String>,
}

/// Reads `~/.claude.json` and the `.mcp.json` of its projects and of
/// `repos` (folders tab knows).
pub fn read_sources(home: Option<&Path>, repos: &[PathBuf], claude_ai: Vec<String>) -> Sources {
    let read = |p: &Path| -> Option<Value> {
        let s = std::fs::read_to_string(p).ok()?;
        serde_json::from_str(&s).ok()
    };
    let claude_json = home.and_then(|h| read(&h.join(".claude.json")));
    let mut folders: Vec<PathBuf> = repos.to_vec();
    if let Some(projects) = claude_json.as_ref().and_then(|c| c["projects"].as_object()) {
        folders.extend(projects.keys().map(PathBuf::from));
    }
    let mut seen = BTreeSet::new();
    let mut mcp_json = Vec::new();
    for f in folders {
        let canon = folder_key(&f);
        if !seen.insert(canon) {
            continue;
        }
        if let Some(v) = read(&f.join(".mcp.json")) {
            mcp_json.push((f, v));
        }
    }
    Sources {
        claude_json,
        mcp_json,
        claude_ai,
    }
}

/// A folder as a key: forward slashes, no trailing slash, and on Windows
/// any case.
fn folder_key(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    let s = s.trim_end_matches('/').to_string();
    if cfg!(windows) {
        s.to_ascii_lowercase()
    } else {
        s
    }
}

/// Everything importable, with what tab already has marked.
pub fn scan(
    src: &Sources,
    existing: &BTreeMap<String, RemoteMcpConfig>,
    secrets: impl Fn(&str) -> Secrets,
    home: Option<&Path>,
    env: impl Fn(&str) -> Option<String>,
) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = Vec::new();
    let mut add = |c: Option<Candidate>| out.extend(c);

    if let Some(cj) = &src.claude_json {
        for (name, def) in servers(&cj["mcpServers"]) {
            add(from_def("user", None, name, def, home, &env));
        }
        if let Some(projects) = cj["projects"].as_object() {
            for (path, p) in projects {
                for (name, def) in servers(&p["mcpServers"]) {
                    add(from_def("project", Some(path), name, def, home, &env));
                }
            }
        }
    }
    for (path, v) in &src.mcp_json {
        let path = path.to_string_lossy();
        for (name, def) in servers(&v["mcpServers"]) {
            add(from_def("repo", Some(&path), name, def, home, &env));
        }
    }
    for full in &src.claude_ai {
        out.push(from_claude_ai(full, home));
    }

    // The same server listed twice (user and project, or two copies of a
    // repo): offer it once.
    let mut seen = BTreeSet::new();
    out.retain(|c| seen.insert(fingerprint(c)));

    // Match what tab has, then give the rest ids that don't clash.
    let mut taken: Vec<String> = existing.keys().cloned().collect();
    for c in &mut out {
        if c.status == Status::Unsupported {
            continue;
        }
        if let Some((id, same)) = matching(c, existing, &secrets) {
            c.existing = Some(id.clone());
            c.id = id;
            c.status = if same {
                Status::Imported
            } else {
                Status::Changed
            };
        }
    }
    for c in &mut out {
        if c.existing.is_none() {
            c.id = suggest_id(&c.id, &taken);
            taken.push(c.id.clone());
        }
    }
    out.sort_by_key(|c| {
        (
            c.status as u8,
            c.source != "user",
            c.name.to_ascii_lowercase(),
        )
    });
    out
}

fn servers(v: &Value) -> Vec<(&String, &Value)> {
    v.as_object()
        .map(|o| o.iter().collect())
        .unwrap_or_default()
}

fn fingerprint(c: &Candidate) -> String {
    match &c.config {
        Some(cfg) => format!(
            "{}|{}|{:?}|{:?}",
            c.name,
            cfg.url,
            cfg.command.as_ref().map(|l| (&l.command, &l.args, &l.env)),
            c.secrets
        ),
        None => c.key.clone(),
    }
}

/// The tab connection a candidate is, and whether it's unchanged.
fn matching(
    c: &Candidate,
    existing: &BTreeMap<String, RemoteMcpConfig>,
    secrets: &impl Fn(&str) -> Secrets,
) -> Option<(String, bool)> {
    let cfg = c.config.as_ref()?;
    if let Some((id, e)) = existing
        .iter()
        .find(|(_, e)| e.imported_from.as_deref() == Some(&c.key))
    {
        let same = e.url == cfg.url
            && e.headers == cfg.headers
            && e.command == cfg.command
            && secrets(id) == c.secrets;
        return Some((id.clone(), same));
    }
    // Added in tab by hand (or from the catalog): it's already there.
    let same_url = |e: &RemoteMcpConfig| {
        !cfg.url.is_empty() && e.url.trim_end_matches('/') == cfg.url.trim_end_matches('/')
    };
    let same_command = |e: &RemoteMcpConfig| {
        cfg.command.is_some()
            && e.command.as_ref().map(|l| (&l.command, &l.args))
                == cfg.command.as_ref().map(|l| (&l.command, &l.args))
    };
    let same_catalog = |e: &RemoteMcpConfig| c.catalog.is_some() && e.catalog == c.catalog;
    existing
        .iter()
        .find(|(_, e)| same_url(e) || same_command(e) || same_catalog(e))
        .map(|(id, _)| (id.clone(), true))
}

fn catalog_for_url(url: &str) -> Option<&'static str> {
    let u = url.trim_end_matches('/');
    catalog()
        .into_iter()
        .find(|e| e.url.trim_end_matches('/') == u)
        .map(|e| e.id)
}

fn from_claude_ai(full: &str, home: Option<&Path>) -> Candidate {
    let name = full.strip_prefix("claude.ai ").unwrap_or(full).trim();
    let lower = name.to_ascii_lowercase();
    let entry = catalog().into_iter().find(|e| {
        lower == e.id
            || lower == e.name.to_ascii_lowercase()
            || lower.split([' ', '-']).any(|w| w == e.id)
            || (e.id == "atlassian" && (lower.contains("jira") || lower.contains("confluence")))
    });
    let mut c = Candidate {
        key: format!("claude_ai:{name}"),
        name: name.to_string(),
        source: "claude_ai",
        path: None,
        kind: "claude_ai",
        url: None,
        command: None,
        header_names: Vec::new(),
        env_names: Vec::new(),
        cloud: CloudFit {
            portable: true,
            why_not: None,
            needs_key: false,
        },
        catalog: None,
        status: Status::New,
        existing: None,
        id: name.to_string(),
        note: None,
        config: None,
        secrets: Secrets::default(),
    };
    match entry {
        Some(e) => {
            let cfg = RemoteMcpConfig {
                name: e.name.to_string(),
                url: e.url.to_string(),
                enabled: true,
                sign_in: true,
                catalog: Some(e.id.to_string()),
                imported_from: Some(c.key.clone()),
                ..Default::default()
            };
            c.cloud = cloud_fit(&cfg, home);
            c.url = Some(e.url.to_string());
            c.catalog = Some(e.id.to_string());
            c.id = e.id.to_string();
            c.config = Some(cfg);
        }
        None => {
            c.status = Status::Unsupported;
            c.note = Some(
                "tab can't read claude.ai connectors' settings, and doesn't know this one. It keeps working in chats that aren't strict."
                    .into(),
            );
        }
    }
    c
}

/// One `mcpServers` entry.
fn from_def(
    source: &'static str,
    path: Option<&str>,
    name: &str,
    def: &Value,
    home: Option<&Path>,
    env: &impl Fn(&str) -> Option<String>,
) -> Option<Candidate> {
    let key = match path {
        Some(p) => format!("{source}:{}:{name}", folder_key(Path::new(p))),
        None => format!("{source}:{name}"),
    };
    let mut missing = BTreeSet::new();
    let mut x = |s: &str| expand(s, env, &mut missing);
    let kind = def["type"]
        .as_str()
        .unwrap_or(if def["command"].is_string() {
            "stdio"
        } else {
            "http"
        });
    let mut c = Candidate {
        key: key.clone(),
        name: name.to_string(),
        source,
        path: path.map(str::to_string),
        kind: "remote",
        url: None,
        command: None,
        header_names: Vec::new(),
        env_names: Vec::new(),
        cloud: CloudFit {
            portable: false,
            why_not: None,
            needs_key: false,
        },
        catalog: None,
        status: Status::New,
        existing: None,
        id: name.to_string(),
        note: None,
        config: None,
        secrets: Secrets::default(),
    };
    let unsupported = |mut c: Candidate, why: String| {
        c.status = Status::Unsupported;
        c.note = Some(why);
        Some(c)
    };
    let cfg = match kind {
        "stdio" => {
            let command = x(def["command"].as_str()?);
            let args: Vec<String> = def["args"]
                .as_array()
                .map(|a| a.iter().filter_map(|v| v.as_str()).map(&mut x).collect())
                .unwrap_or_default();
            let mut vars = BTreeMap::new();
            for (k, v) in def["env"].as_object().into_iter().flatten() {
                if let Some(v) = v.as_str() {
                    vars.insert(k.clone(), x(v));
                }
            }
            c.kind = "local";
            c.command = Some(
                std::iter::once(command.clone())
                    .chain(args.iter().map(|a| quote(a)))
                    .collect::<Vec<_>>()
                    .join(" "),
            );
            c.env_names = vars.keys().cloned().collect();
            let cfg = RemoteMcpConfig {
                name: name.to_string(),
                enabled: true,
                command: Some(LocalCommand {
                    command,
                    args,
                    env: vars.keys().cloned().collect(),
                    cwd: path.map(str::to_string),
                }),
                imported_from: Some(key),
                ..Default::default()
            };
            c.secrets.env = vars;
            cfg
        }
        "http" | "streamable-http" => {
            let url = x(def["url"].as_str()?);
            c.url = Some(url.clone());
            if let Err(e) = super::check_url(&url) {
                return unsupported(c, e.to_string());
            }
            let mut headers = BTreeMap::new();
            for (k, v) in def["headers"].as_object().into_iter().flatten() {
                if let Some(v) = v.as_str() {
                    headers.insert(k.clone(), x(v));
                }
            }
            if def["headersHelper"].is_string() {
                return unsupported(
                    c,
                    "It gets its headers from a script (headersHelper), which tab doesn't run."
                        .into(),
                );
            }
            c.kind = if headers.is_empty() {
                "remote"
            } else {
                "remote_keys"
            };
            c.header_names = headers.keys().cloned().collect();
            c.catalog = catalog_for_url(&url).map(str::to_string);
            let cfg = RemoteMcpConfig {
                name: name.to_string(),
                url,
                enabled: true,
                catalog: c.catalog.clone(),
                headers: headers.keys().cloned().collect(),
                imported_from: Some(key),
                ..Default::default()
            };
            c.secrets.headers = headers;
            cfg
        }
        "sse" => {
            c.url = def["url"].as_str().map(str::to_string);
            return unsupported(
                c,
                "It uses the older SSE transport, which tab doesn't speak yet.".into(),
            );
        }
        other => return unsupported(c, format!("tab doesn't know \"{other}\" servers.")),
    };
    if !missing.is_empty() {
        c.note = Some(format!(
            "{} isn't set for tab, so it'll be empty.",
            missing.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }
    c.cloud = cloud_fit(&cfg, home);
    if let Some(cat) = &c.catalog {
        c.id = cat.clone();
    }
    c.config = Some(cfg);
    Some(c)
}

fn quote(a: &str) -> String {
    if a.is_empty() || a.contains([' ', '"', '\'']) {
        format!("\"{}\"", a.replace('"', "\\\""))
    } else {
        a.to_string()
    }
}

/// Claude Code's `${VAR}` and `${VAR:-default}` in `.mcp.json`.
fn expand(
    s: &str,
    env: &impl Fn(&str) -> Option<String>,
    missing: &mut BTreeSet<String>,
) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find("${") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 2..];
        let Some(end) = after.find('}') else {
            out.push_str(&rest[i..]);
            return out;
        };
        let inner = &after[..end];
        let (var, default) = match inner.split_once(":-") {
            Some((v, d)) => (v, Some(d)),
            None => (inner, None),
        };
        match env(var).or_else(|| default.map(str::to_string)) {
            Some(v) => out.push_str(&v),
            None => {
                missing.insert(var.to_string());
            }
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    fn sources() -> Sources {
        Sources {
            claude_json: Some(json!({
                "mcpServers": {
                    "linear": { "type": "http", "url": "https://mcp.linear.app/mcp" },
                    "github": { "type": "http", "url": "https://api.githubcopilot.com/mcp/", "headers": { "Authorization": "Bearer ghp_x" } },
                    "fs": { "command": "npx", "args": ["-y", "@modelcontextprotocol/server-filesystem", "/Users/me/notes"] },
                    "old": { "type": "sse", "url": "https://example.com/sse" }
                },
                "projects": {
                    "/Users/me/app": { "mcpServers": {
                        "db": { "type": "stdio", "command": "uvx", "args": ["mcp-db", "--url", "postgres://localhost/app"], "env": { "DB_PASSWORD": "pw" } }
                    } }
                }
            })),
            mcp_json: vec![(
                PathBuf::from("/Users/me/app"),
                json!({ "mcpServers": {
                    "sentry": { "type": "http", "url": "https://mcp.sentry.dev/mcp" },
                    "docs": { "command": "npx", "args": ["-y", "docs-mcp"], "env": { "DOCS_KEY": "${DOCS_KEY}", "REGION": "${REGION:-us}" } }
                } }),
            )],
            claude_ai: vec!["claude.ai Notion".into(), "claude.ai Health".into()],
        }
    }

    fn by_name<'a>(cs: &'a [Candidate], name: &str) -> &'a Candidate {
        cs.iter().find(|c| c.name == name).unwrap()
    }

    #[test]
    fn reads_every_kind_of_server() {
        let home = Path::new("/Users/me");
        let cs = scan(
            &sources(),
            &BTreeMap::new(),
            |_| Secrets::default(),
            Some(home),
            no_env,
        );
        assert_eq!(cs.len(), 9);

        let linear = by_name(&cs, "linear");
        assert_eq!((linear.kind, linear.status), ("remote", Status::New));
        assert_eq!(linear.catalog.as_deref(), Some("linear"));
        assert!(linear.cloud.portable);

        // Keys go to the vault; the dialog sees their names.
        let gh = by_name(&cs, "github");
        assert_eq!(gh.kind, "remote_keys");
        assert_eq!(gh.header_names, ["Authorization"]);
        assert_eq!(gh.secrets.headers["Authorization"], "Bearer ghp_x");
        assert!(gh.cloud.needs_key);
        assert!(!serde_json::to_string(gh).unwrap().contains("ghp_x"));

        // A path under home ties it to this computer.
        let fs = by_name(&cs, "fs");
        assert_eq!(fs.kind, "local");
        assert!(!fs.cloud.portable);
        assert_eq!(
            fs.command.as_deref(),
            Some("npx -y @modelcontextprotocol/server-filesystem /Users/me/notes")
        );

        // localhost does too, and the project's servers run in the project.
        let db = by_name(&cs, "db");
        assert_eq!(db.source, "project");
        assert!(!db.cloud.portable);
        assert_eq!(
            db.config
                .as_ref()
                .unwrap()
                .command
                .as_ref()
                .unwrap()
                .cwd
                .as_deref(),
            Some("/Users/me/app")
        );
        assert_eq!(db.env_names, ["DB_PASSWORD"]);

        // ${VAR} expands; what's missing is said.
        let docs = by_name(&cs, "docs");
        assert_eq!(docs.source, "repo");
        assert!(docs.cloud.portable);
        assert_eq!(docs.secrets.env["REGION"], "us");
        assert_eq!(
            docs.note.as_deref(),
            Some("DOCS_KEY isn't set for tab, so it'll be empty.")
        );

        assert_eq!(by_name(&cs, "old").status, Status::Unsupported);
        let notion = by_name(&cs, "Notion");
        assert_eq!(
            (notion.status, notion.catalog.as_deref()),
            (Status::New, Some("notion"))
        );
        assert!(notion.config.as_ref().unwrap().sign_in);
        assert_eq!(by_name(&cs, "Health").status, Status::Unsupported);

        // Importable ones come first.
        assert!(cs.iter().take(7).all(|c| c.actionable()));
    }

    #[test]
    fn knows_what_tab_already_has() {
        let home = Path::new("/Users/me");
        let first = scan(
            &sources(),
            &BTreeMap::new(),
            |_| Secrets::default(),
            Some(home),
            no_env,
        );
        // As if every importable one was imported, with Linear added by hand.
        let mut existing: BTreeMap<String, RemoteMcpConfig> = first
            .iter()
            .filter(|c| c.actionable() && c.name != "linear")
            .map(|c| (c.id.clone(), c.config.clone().unwrap()))
            .collect();
        existing.insert(
            "linear".into(),
            RemoteMcpConfig {
                name: "Linear".into(),
                url: "https://mcp.linear.app/mcp".into(),
                sign_in: true,
                catalog: Some("linear".into()),
                ..Default::default()
            },
        );
        let vault: BTreeMap<String, Secrets> = first
            .iter()
            .map(|c| (c.id.clone(), c.secrets.clone()))
            .collect();
        let secrets = |id: &str| vault.get(id).cloned().unwrap_or_default();
        let again = scan(&sources(), &existing, secrets, Some(home), no_env);
        assert!(again.iter().all(|c| !c.actionable()), "{again:#?}");
        assert_eq!(
            by_name(&again, "linear").existing.as_deref(),
            Some("linear")
        );

        // A key changed in Claude Code: that one is "changed".
        let mut src = sources();
        src.claude_json.as_mut().unwrap()["mcpServers"]["github"]["headers"]["Authorization"] =
            json!("Bearer new");
        let changed = scan(&src, &existing, secrets, Some(home), no_env);
        let gh = by_name(&changed, "github");
        assert_eq!(gh.status, Status::Changed);
        assert_eq!(gh.id, gh.existing.clone().unwrap());
    }

    #[test]
    fn ids_dont_clash() {
        let mut existing = BTreeMap::new();
        existing.insert(
            "fs".to_string(),
            RemoteMcpConfig {
                name: "x".into(),
                url: "https://x.dev/mcp".into(),
                ..Default::default()
            },
        );
        let cs = scan(&sources(), &existing, |_| Secrets::default(), None, no_env);
        assert_eq!(by_name(&cs, "fs").id, "fs-2");
    }

    #[test]
    fn expands_variables() {
        let env = |k: &str| (k == "A").then(|| "1".to_string());
        let mut missing = BTreeSet::new();
        assert_eq!(expand("x${A}y${B:-2}z${C}", &env, &mut missing), "x1y2z");
        assert_eq!(missing.into_iter().collect::<Vec<_>>(), ["C"]);
    }

    #[test]
    fn cloud_fit_rules() {
        let url = |u: &str| RemoteMcpConfig {
            url: u.into(),
            ..Default::default()
        };
        let cmd = |c: &str, a: &[&str]| RemoteMcpConfig {
            command: Some(LocalCommand {
                command: c.into(),
                args: a.iter().map(|s| s.to_string()).collect(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let ok = |c: &RemoteMcpConfig| cloud_fit(c, Some(Path::new("/home/me"))).portable;
        assert!(ok(&url("https://mcp.linear.app/mcp")));
        assert!(!ok(&url("http://localhost:3000/mcp")));
        assert!(!ok(&url("https://10.0.0.5/mcp")));
        assert!(!ok(&url("https://printer.local/mcp")));
        assert!(!ok(&url("http://[::1]:8080/mcp")));
        assert!(ok(&cmd("npx", &["-y", "@acme/mcp"])));
        assert!(ok(&cmd("uvx", &["mcp-server-fetch"])));
        assert!(!ok(&cmd("/usr/local/bin/my-mcp", &[])));
        assert!(!ok(&cmd("my-mcp", &[])));
        assert!(!ok(&cmd("npx", &["-y", "fs-mcp", "C:\\Users\\me\\notes"])));
        assert!(!ok(&cmd("npx", &["-y", "fs-mcp", "--root=~/notes"])));
        assert!(!ok(&cmd(
            "npx",
            &["-y", "pg-mcp", "postgres://192.168.1.4/db"]
        )));
    }
}
