//! Language servers for the review workspace.
//!
//! Monaco has real language features only for TypeScript and JavaScript.
//! For everything else tab runs a language server (gopls, sqls,
//! rust-analyzer, pyright) against the PR's local checkout and relays LSP
//! between it and the editor. The UI owns the protocol (it builds requests
//! and reads results); this module finds, installs, starts and frames.
//!
//! Servers are found on PATH and in the places their installers put them,
//! because a GUI app on macOS doesn't inherit the shell's PATH.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use serde::Serialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::ChildStdin;
use tokio::sync::{Mutex, oneshot};

/// One way to install a server, tried when its `needs` tool is present.
#[derive(Debug, Clone, Serialize)]
pub struct Installer {
    /// The tool that runs the install (`go`, `brew`, `npm`, `rustup`).
    pub needs: &'static str,
    pub args: &'static [&'static str],
    /// Shown when `needs` is missing.
    pub needs_url: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct ServerSpec {
    pub id: &'static str,
    pub name: &'static str,
    /// Monaco language ids this server handles.
    pub languages: &'static [&'static str],
    pub command: &'static str,
    pub args: &'static [&'static str],
    pub installers: &'static [Installer],
    /// What to expect, shown in the UI.
    pub note: &'static str,
}

pub const SERVERS: &[ServerSpec] = &[
    ServerSpec {
        id: "gopls",
        name: "Go (gopls)",
        languages: &["go"],
        command: "gopls",
        args: &[],
        installers: &[
            Installer {
                needs: "go",
                args: &["install", "golang.org/x/tools/gopls@latest"],
                needs_url: "https://go.dev/dl/",
            },
            Installer {
                needs: "brew",
                args: &["install", "gopls"],
                needs_url: "https://brew.sh",
            },
        ],
        note: "Hover, go to definition, references and errors across the module.",
    },
    ServerSpec {
        id: "sqls",
        name: "SQL (sqls)",
        languages: &["sql"],
        command: "sqls",
        args: &[],
        installers: &[Installer {
            needs: "go",
            args: &["install", "github.com/sqls-server/sqls@latest"],
            needs_url: "https://go.dev/dl/",
        }],
        note: "Syntax errors and outline. Table and column info needs a database connection in sqls's own config.",
    },
    ServerSpec {
        id: "rust-analyzer",
        name: "Rust (rust-analyzer)",
        languages: &["rust"],
        command: "rust-analyzer",
        args: &[],
        installers: &[
            Installer {
                needs: "rustup",
                args: &["component", "add", "rust-analyzer"],
                needs_url: "https://rustup.rs",
            },
            Installer {
                needs: "brew",
                args: &["install", "rust-analyzer"],
                needs_url: "https://brew.sh",
            },
        ],
        note: "Hover, go to definition and references. Indexing a large workspace takes a minute.",
    },
    ServerSpec {
        id: "pyright",
        name: "Python (pyright)",
        languages: &["python"],
        command: "pyright-langserver",
        args: &["--stdio"],
        installers: &[Installer {
            needs: "npm",
            args: &["install", "-g", "pyright"],
            needs_url: "https://nodejs.org",
        }],
        note: "Hover, go to definition, references and type errors.",
    },
];

/// A server's own settings, sent at start and whenever it asks.
///
/// rust-analyzer runs a crate's build scripts and proc macros by default,
/// which is arbitrary code from the pull request. They stay on only where
/// the sandbox also holds the network to package registries (macOS);
/// elsewhere they're off, at the cost of macro-generated items.
fn server_settings(id: &str, contained: bool) -> Value {
    match id {
        "rust-analyzer" => json!({
            "cargo": { "buildScripts": { "enable": contained } },
            "procMacro": { "enable": contained },
            "checkOnSave": false,
        }),
        _ => Value::Null,
    }
}

pub fn spec(id: &str) -> Option<&'static ServerSpec> {
    SERVERS.iter().find(|s| s.id == id)
}

fn home() -> Option<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from)
}

/// Directories installers commonly use, beyond PATH.
fn extra_dirs() -> Vec<PathBuf> {
    let mut d = Vec::new();
    if let Some(gobin) = std::env::var_os("GOBIN") {
        d.push(PathBuf::from(gobin));
    }
    if let Some(gopath) = std::env::var_os("GOPATH") {
        d.push(PathBuf::from(gopath).join("bin"));
    }
    if let Some(h) = home() {
        d.push(h.join("go").join("bin"));
        d.push(h.join(".cargo").join("bin"));
        d.push(h.join(".local").join("bin"));
    }
    if cfg!(windows) {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            d.push(PathBuf::from(appdata).join("npm"));
        }
        for v in ["ProgramFiles", "LOCALAPPDATA"] {
            if let Some(p) = std::env::var_os(v) {
                d.push(PathBuf::from(&p).join("Go").join("bin"));
                d.push(PathBuf::from(&p).join("nodejs"));
            }
        }
    } else {
        for p in [
            "/opt/homebrew/bin",
            "/usr/local/bin",
            "/usr/local/go/bin",
            "/usr/bin",
            "/home/linuxbrew/.linuxbrew/bin",
        ] {
            d.push(PathBuf::from(p));
        }
    }
    d
}

/// Finds a program on PATH or in the usual install locations.
pub fn find(name: &str) -> Option<PathBuf> {
    if let Ok(p) = which::which(name) {
        return Some(p);
    }
    let exts: &[&str] = if cfg!(windows) {
        &[".exe", ".cmd", ".bat", ""]
    } else {
        &[""]
    };
    extra_dirs()
        .into_iter()
        .flat_map(|d| exts.iter().map(move |e| d.join(format!("{name}{e}"))))
        .find(|p| p.is_file())
}

/// PATH with the usual install locations added, so tools a server shells
/// out to (gopls runs `go`) are found too.
fn path_env() -> std::ffi::OsString {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    dirs.extend(extra_dirs());
    std::env::join_paths(dirs).unwrap_or_default()
}

fn command(exe: &Path) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(exe);
    #[cfg(windows)]
    {
        cmd.creation_flags(0x0800_0000);
    }
    cmd.env("PATH", path_env()).kill_on_drop(true);
    cmd
}

#[derive(Debug, Clone, Serialize)]
pub struct ServerStatus {
    #[serde(flatten)]
    pub spec: ServerSpec,
    pub installed: bool,
    pub path: Option<String>,
    /// The command tab would run to install it, if a tool for it exists here.
    pub install_command: Option<String>,
    /// When nothing can install it: what to get first.
    pub needs: Option<String>,
    pub needs_url: Option<String>,
}

pub fn status() -> Vec<ServerStatus> {
    SERVERS
        .iter()
        .map(|s| {
            let path = find(s.command);
            let inst = s.installers.iter().find(|i| find(i.needs).is_some());
            let first = s.installers.first();
            ServerStatus {
                spec: s.clone(),
                installed: path.is_some(),
                path: path.map(|p| p.display().to_string()),
                install_command: inst.map(|i| format!("{} {}", i.needs, i.args.join(" "))),
                needs: inst
                    .is_none()
                    .then(|| first.map(|i| i.needs.to_string()))
                    .flatten(),
                needs_url: inst
                    .is_none()
                    .then(|| first.map(|i| i.needs_url.to_string()))
                    .flatten(),
            }
        })
        .collect()
}

/// Installs a server with the first installer whose tool is present.
pub async fn install(id: &str) -> anyhow::Result<ServerStatus> {
    let s = spec(id).ok_or_else(|| anyhow::anyhow!("unknown language server {id}"))?;
    let (inst, exe) = s
        .installers
        .iter()
        .find_map(|i| find(i.needs).map(|e| (i, e)))
        .ok_or_else(|| {
            let i = &s.installers[0];
            anyhow::anyhow!(
                "Installing {} needs {} first: {}",
                s.name,
                i.needs,
                i.needs_url
            )
        })?;
    let out = tokio::time::timeout(
        Duration::from_secs(600),
        command(&exe).args(inst.args).stdin(Stdio::null()).output(),
    )
    .await
    .map_err(|_| anyhow::anyhow!("The install took more than 10 minutes and was stopped."))??;
    let st = status()
        .into_iter()
        .find(|x| x.spec.id == id)
        .expect("known id");
    if !out.status.success() || !st.installed {
        let text = String::from_utf8_lossy(if out.stderr.is_empty() {
            &out.stdout
        } else {
            &out.stderr
        })
        .lines()
        .rfind(|l| !l.trim().is_empty())
        .unwrap_or("")
        .to_string();
        anyhow::bail!(
            "`{} {}` didn't install {}{}",
            inst.needs,
            inst.args.join(" "),
            s.command,
            if text.is_empty() {
                ".".into()
            } else {
                format!(": {text}")
            }
        );
    }
    Ok(st)
}

/// A `file://` URI for a local path, the way language servers expect it.
pub fn file_uri(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    let mut out = String::from("file://");
    if !s.starts_with('/') {
        out.push('/');
    }
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b':' if out.len() <= 10 => out.push(':'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

type Pending = Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value, Value>>>>>;

/// A running server.
pub struct Session {
    pub id: String,
    pub server: &'static str,
    pub root_uri: String,
    /// Set once `initialize` answers.
    capabilities: std::sync::OnceLock<Value>,
    alive: std::sync::atomic::AtomicBool,
    stdin: Mutex<ChildStdin>,
    pending: Pending,
    next: AtomicI64,
    child: Mutex<crate::sandbox::Guarded>,
}

async fn write_msg(w: &mut ChildStdin, v: &Value) -> std::io::Result<()> {
    let body = serde_json::to_vec(v)?;
    w.write_all(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes())
        .await?;
    w.write_all(&body).await?;
    w.flush().await
}

impl Session {
    pub fn capabilities(&self) -> Value {
        self.capabilities.get().cloned().unwrap_or(Value::Null)
    }

    pub fn alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }

    pub async fn request(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        write_msg(
            &mut *self.stdin.lock().await,
            &json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }),
        )
        .await?;
        match tokio::time::timeout(Duration::from_secs(30), rx).await {
            Ok(Ok(Ok(v))) => Ok(v),
            Ok(Ok(Err(e))) => anyhow::bail!(
                "{}",
                e.get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("language server error")
            ),
            Ok(Err(_)) => anyhow::bail!("{} stopped", self.server),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                anyhow::bail!("{} didn't answer {method} in time", self.server)
            }
        }
    }

    pub async fn notify(&self, method: &str, params: Value) -> anyhow::Result<()> {
        write_msg(
            &mut *self.stdin.lock().await,
            &json!({ "jsonrpc": "2.0", "method": method, "params": params }),
        )
        .await?;
        Ok(())
    }

    pub async fn stop(&self) {
        let _ = tokio::time::timeout(
            Duration::from_secs(2),
            self.request("shutdown", Value::Null),
        )
        .await;
        let _ = self.notify("exit", Value::Null).await;
        self.child.lock().await.kill_tree();
    }
}

/// A notification from a server, for the UI (diagnostics, mostly).
#[derive(Debug, Clone, Serialize)]
pub struct LspEvent {
    pub session: String,
    pub method: String,
    pub params: Value,
}

#[derive(Default)]
pub struct LspHub {
    sessions: Mutex<HashMap<String, Arc<Session>>>,
}

impl LspHub {
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts (or reuses) a server for a checkout.
    pub async fn start(
        &self,
        server: &str,
        root: &Path,
        on_event: impl Fn(LspEvent) + Send + Sync + 'static,
    ) -> anyhow::Result<Arc<Session>> {
        let s = spec(server).ok_or_else(|| anyhow::anyhow!("unknown language server {server}"))?;
        let root_uri = file_uri(root);
        let id = format!("{}:{}", s.id, root_uri);
        if let Some(existing) = self.sessions.lock().await.get(&id).filter(|s| s.alive()) {
            return Ok(existing.clone());
        }
        let exe =
            find(s.command).ok_or_else(|| anyhow::anyhow!("{} isn't installed", s.command))?;
        // The checkout is someone else's code, and language servers run
        // parts of it (build scripts, proc macros, `go list`). They get the
        // checkout and build caches to write to, no credentials, and only
        // package registries on the network.
        let sb = crate::sandbox::current();
        let policy = sb.isolated_policy(
            &[root.to_path_buf()],
            crate::sandbox::Network::from_list(&sb.config().lsp_domains),
        );
        let settings = server_settings(
            s.id,
            sb.backend() == crate::sandbox::Backend::Seatbelt && sb.enforced(),
        );
        let args: Vec<std::ffi::OsString> = s.args.iter().map(Into::into).collect();
        let mut prepared = sb.command(&exe, &args, root, &policy).await?;
        prepared
            .cmd
            .env("PATH", path_env())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = prepared.spawn()?;
        let stdin = child.stdin.take().expect("piped");
        let stdout = child.stdout.take().expect("piped");
        let stderr = child.stderr.take().expect("piped");
        let pending: Pending = Arc::default();

        // Server stderr is just logs.
        let name = s.id;
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                tracing::debug!(server = name, "{l}");
            }
        });

        let session = Arc::new(Session {
            id: id.clone(),
            server: s.id,
            root_uri: root_uri.clone(),
            capabilities: std::sync::OnceLock::new(),
            alive: std::sync::atomic::AtomicBool::new(true),
            stdin: Mutex::new(stdin),
            pending: pending.clone(),
            next: AtomicI64::new(1),
            child: Mutex::new(child),
        });

        // Reader: responses go to their waiters, server requests get a
        // default answer, notifications go to the UI.
        let init_settings = settings.clone();
        let reader_session = Arc::downgrade(&session);
        let sid = id.clone();
        tokio::spawn(async move {
            let mut r = BufReader::new(stdout);
            loop {
                let Ok(Some(msg)) = read_msg(&mut r).await else {
                    break;
                };
                if let Some(rid) = msg
                    .get("id")
                    .and_then(Value::as_i64)
                    .filter(|_| msg.get("method").is_none())
                {
                    if let Some(tx) = pending.lock().await.remove(&rid) {
                        let _ = tx.send(match msg.get("error") {
                            Some(e) if !e.is_null() => Err(e.clone()),
                            _ => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
                        });
                    }
                    continue;
                }
                let method = msg
                    .get("method")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                if let Some(req_id) = msg.get("id").cloned() {
                    // workspace/configuration wants one item per request; others take null.
                    let result = if method == "workspace/configuration" {
                        let items = msg["params"]["items"]
                            .as_array()
                            .cloned()
                            .unwrap_or_default();
                        Value::Array(
                            items
                                .iter()
                                .map(|i| match i["section"].as_str() {
                                    Some(sec) if !settings.is_null() && sec == name => {
                                        settings.clone()
                                    }
                                    _ => Value::Null,
                                })
                                .collect(),
                        )
                    } else {
                        Value::Null
                    };
                    if let Some(s) = reader_session.upgrade() {
                        let _ = write_msg(
                            &mut *s.stdin.lock().await,
                            &json!({ "jsonrpc": "2.0", "id": req_id, "result": result }),
                        )
                        .await;
                    }
                    continue;
                }
                on_event(LspEvent {
                    session: sid.clone(),
                    method,
                    params: msg.get("params").cloned().unwrap_or(Value::Null),
                });
            }
            // Fail anything still waiting, and let the UI know.
            pending.lock().await.clear();
            if let Some(s) = reader_session.upgrade() {
                s.alive.store(false, Ordering::Relaxed);
            }
            on_event(LspEvent {
                session: sid.clone(),
                method: "tab/exited".into(),
                params: Value::Null,
            });
        });

        let init = session
            .request(
                "initialize",
                json!({
                    "processId": std::process::id(),
                    "clientInfo": { "name": "tab" },
                    "rootUri": root_uri,
                    "initializationOptions": init_settings,
                    "workspaceFolders": [{ "uri": root_uri, "name": root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default() }],
                    "capabilities": {
                        "general": { "positionEncodings": ["utf-16"] },
                        "textDocument": {
                            "synchronization": { "didSave": false, "dynamicRegistration": false },
                            "hover": { "contentFormat": ["markdown", "plaintext"] },
                            "definition": { "linkSupport": true },
                            "typeDefinition": { "linkSupport": true },
                            "implementation": { "linkSupport": true },
                            "references": {},
                            "documentSymbol": { "hierarchicalDocumentSymbolSupport": true },
                            "documentHighlight": {},
                            "publishDiagnostics": { "relatedInformation": false },
                        },
                        "workspace": { "workspaceFolders": true, "configuration": true },
                        "window": { "workDoneProgress": false },
                    },
                }),
            )
            .await;
        let caps = match init {
            Ok(v) => v.get("capabilities").cloned().unwrap_or(Value::Null),
            Err(e) => {
                session.stop().await;
                return Err(e.context(format!("{} didn't start", s.name)));
            }
        };
        session.notify("initialized", json!({})).await?;
        let _ = session.capabilities.set(caps);
        self.sessions.lock().await.insert(id, session.clone());
        Ok(session)
    }

    pub async fn get(&self, id: &str) -> Option<Arc<Session>> {
        self.sessions.lock().await.get(id).cloned()
    }

    pub async fn stop(&self, id: &str) {
        let s = self.sessions.lock().await.remove(id);
        if let Some(s) = s {
            s.stop().await;
        }
    }

    pub async fn stop_all(&self) {
        let all: Vec<_> = self.sessions.lock().await.drain().map(|(_, s)| s).collect();
        for s in all {
            s.stop().await;
        }
    }
}

async fn read_msg<R: tokio::io::AsyncBufRead + Unpin>(r: &mut R) -> std::io::Result<Option<Value>> {
    let mut len = None;
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).await? == 0 {
            return Ok(None);
        }
        let line = line.trim_end();
        if line.is_empty() {
            if len.is_some() {
                break;
            }
            continue;
        }
        if let Some(v) = line.strip_prefix("Content-Length:") {
            len = v.trim().parse::<usize>().ok();
        }
    }
    let mut buf = vec![0u8; len.unwrap_or(0)];
    r.read_exact(&mut buf).await?;
    Ok(serde_json::from_slice(&buf).ok().or(Some(Value::Null)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn makes_file_uris() {
        assert_eq!(
            file_uri(Path::new("/Volumes/Source/my repo")),
            "file:///Volumes/Source/my%20repo"
        );
        assert_eq!(file_uri(Path::new(r"D:\tab\pr-1")), "file:///D:/tab/pr-1");
    }

    #[tokio::test]
    async fn reads_framed_messages() {
        let data = b"Content-Length: 17\r\n\r\n{\"jsonrpc\":\"2.0\"}Content-Length: 2\r\nContent-Type: x\r\n\r\n{}";
        let mut r = BufReader::new(&data[..]);
        assert_eq!(
            read_msg(&mut r).await.unwrap(),
            Some(json!({ "jsonrpc": "2.0" }))
        );
        assert_eq!(read_msg(&mut r).await.unwrap(), Some(json!({})));
        assert_eq!(read_msg(&mut r).await.unwrap(), None);
    }

    #[test]
    fn every_server_has_an_installer() {
        for s in SERVERS {
            assert!(!s.installers.is_empty(), "{}", s.id);
            assert!(!s.languages.is_empty(), "{}", s.id);
        }
    }

    /// Runs only where gopls is installed.
    #[tokio::test]
    async fn talks_to_gopls() {
        if find("gopls").is_none() {
            eprintln!("gopls not installed; skipping");
            return;
        }
        let dir = std::env::temp_dir().join(format!("tab-lsp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("go.mod"), "module example.com/m\n\ngo 1.21\n").unwrap();
        let src = "package main\n\n// Greet says hi.\nfunc Greet() string { return \"hi\" }\n\nfunc main() { _ = Greet() }\n";
        std::fs::write(dir.join("main.go"), src).unwrap();
        let hub = LspHub::new();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let s = hub
            .start("gopls", &dir, move |e| {
                let _ = tx.send(e);
            })
            .await
            .unwrap();
        assert!(s.capabilities().get("hoverProvider").is_some());
        let uri = format!("{}/main.go", s.root_uri);
        s.notify("textDocument/didOpen", json!({ "textDocument": { "uri": uri, "languageId": "go", "version": 1, "text": src } })).await.unwrap();
        let hover = s.request("textDocument/hover", json!({ "textDocument": { "uri": uri }, "position": { "line": 5, "character": 19 } })).await.unwrap();
        assert!(hover.to_string().contains("Greet says hi"), "{hover}");
        let def = s.request("textDocument/definition", json!({ "textDocument": { "uri": uri }, "position": { "line": 5, "character": 19 } })).await.unwrap();
        assert!(def.to_string().contains("main.go"), "{def}");
        // Same root reuses the session.
        let again = hub.start("gopls", &dir, |_| {}).await.unwrap();
        assert!(Arc::ptr_eq(&s, &again));
        hub.stop_all().await;
        let mut exited = false;
        while let Ok(Some(e)) = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
            if e.method == "tab/exited" {
                exited = true;
                break;
            }
        }
        assert!(exited);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Runs only where sqls is installed: it starts without a database.
    #[tokio::test]
    async fn starts_sqls_without_a_database() {
        if find("sqls").is_none() {
            eprintln!("sqls not installed; skipping");
            return;
        }
        let dir = std::env::temp_dir().join(format!("tab-sqls-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let hub = LspHub::new();
        let s = hub.start("sqls", &dir, |_| {}).await.unwrap();
        let uri = format!("{}/q.sql", s.root_uri);
        s.notify("textDocument/didOpen", json!({ "textDocument": { "uri": uri, "languageId": "sql", "version": 1, "text": "SELECT 1;\n" } })).await.unwrap();
        // Any answer (even empty) means it's alive and speaking LSP.
        let _ = s.request("textDocument/hover", json!({ "textDocument": { "uri": uri }, "position": { "line": 0, "character": 2 } })).await;
        assert!(s.alive());
        hub.stop_all().await;
        let _ = std::fs::remove_dir_all(&dir);
    }
}
