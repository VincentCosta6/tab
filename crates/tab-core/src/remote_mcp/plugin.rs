//! Connector plugins: programs a manifest names for what its steps can't
//! express (parsing markdown, logic with conditions). See
//! `docs/recipe-manifests.md`.
//!
//! tab starts the program for each look (and each check) in the isolated
//! sandbox: it reads files that aren't hidden, writes only its own folder
//! and temp, and has no network unless the manifest lists hosts. They talk
//! JSON-RPC 2.0, one message per line on stdin and stdout:
//!
//! - tab asks `poll` (`{choice, query, since}`) or `check`
//!   (`{external_id, metadata}`) and waits for the answer: `{items: [...]}`
//!   or `{resolved: true | false | null}`.
//! - Meanwhile the plugin may ask tab `tools/list` and `tools/call`
//!   (`{name, arguments}`): tab calls the connection's tools as the person,
//!   under the same budgets, audit log and read-only rule as a manifest's
//!   steps. A plugin never sees the connection's sign-in.
//! - A `log` notification (`{message}`) goes to tab's log.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

use super::sync::{Found, Session, time_of};
use crate::model::{Block, EntityRef, Field, IncomingEvent, Link, Tone};
use crate::sandbox::Network;

/// At most this many tool calls per request.
const MAX_CALLS: usize = 50;
/// At most this many items per look.
const MAX_ITEMS: usize = 500;

/// How a manifest names its plugin.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginSpec {
    /// A program on PATH (`node`), or a path from the manifest's folder.
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// Hosts it may reach itself; none by default (tool calls go through
    /// tab either way).
    #[serde(default)]
    pub network: Vec<String>,
    /// How long one look may take.
    #[serde(default = "one_minute")]
    pub timeout: String,
}

fn one_minute() -> String {
    "60s".into()
}

/// One item as a plugin sends it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginItem {
    pub id: String,
    #[serde(default)]
    pub kind: Option<String>,
    pub title: String,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub actor: Option<String>,
    #[serde(default)]
    pub context: Option<String>,
    #[serde(default)]
    pub time: Option<String>,
    #[serde(default)]
    pub resolved: bool,
    #[serde(default)]
    pub fields: Vec<PluginField>,
    #[serde(default)]
    pub badges: bool,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub markdown: bool,
    /// The label of a link to `url`.
    #[serde(default)]
    pub link: Option<String>,
    #[serde(default)]
    pub entities: Vec<PluginEntity>,
    #[serde(default)]
    pub metadata: Value,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginField {
    pub label: String,
    pub value: String,
    #[serde(default)]
    pub tone: Tone,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginEntity {
    pub kind: String,
    pub key: String,
    pub label: String,
    #[serde(default = "rel_in")]
    pub rel: String,
}

fn rel_in() -> String {
    "in".into()
}

impl PluginItem {
    /// As a queue item; `kinds` are the manifest's, the first the default.
    pub fn found(self, kinds: &[&str]) -> anyhow::Result<Found> {
        let kind = self.kind.unwrap_or_else(|| kinds[0].to_string());
        anyhow::ensure!(
            kinds.contains(&kind.as_str()),
            "item kind \"{kind}\" isn't one of the manifest's kinds"
        );
        let fields: Vec<Field> = self
            .fields
            .into_iter()
            .map(|f| Field::new(f.label, f.value).tone(f.tone))
            .collect();
        let mut blocks = Vec::new();
        if !fields.is_empty() {
            blocks.push(if self.badges {
                Block::Badges { items: fields }
            } else {
                Block::Fields { items: fields }
            });
        }
        if let Some(text) = self.text.filter(|t| !t.trim().is_empty()) {
            blocks.push(Block::Text {
                text,
                markdown: self.markdown,
            });
        }
        if let (Some(label), Some(url)) = (self.link, &self.url) {
            blocks.push(Block::Links {
                items: vec![Link {
                    label,
                    url: url.clone(),
                }],
            });
        }
        Ok(Found {
            resolved: self.resolved,
            event: IncomingEvent {
                source: String::new(),
                external_id: self.id,
                kind,
                title: self.title,
                body: self.body,
                url: self.url,
                actor: self.actor,
                context: self.context,
                occurred_at: self
                    .time
                    .and_then(|t| time_of(&Value::String(t)))
                    .unwrap_or_else(Utc::now),
                blocks,
                entities: self
                    .entities
                    .into_iter()
                    .map(|e| EntityRef {
                        kind: e.kind,
                        key: e.key,
                        label: e.label,
                        rel: e.rel,
                    })
                    .collect(),
                metadata: if self.metadata.is_object() {
                    self.metadata
                } else {
                    json!({})
                },
            },
        })
    }
}

/// A plugin, ready to run.
pub struct Plugin {
    pub name: &'static str,
    pub spec: &'static PluginSpec,
    /// The manifest's folder: where it starts, and what it may write.
    pub dir: PathBuf,
    pub timeout: Duration,
    /// Shipped with tab: may call any tool.
    pub built_in: bool,
}

impl Plugin {
    pub async fn poll(
        &self,
        s: &Session<'_>,
        kinds: &[&str],
        since: Option<DateTime<Utc>>,
    ) -> anyhow::Result<Vec<Found>> {
        let r = self
            .request(
                s,
                "poll",
                json!({ "choice": s.choice, "query": s.query, "since": since.map(|t| t.to_rfc3339()) }),
            )
            .await?;
        let items = r["items"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("{} answered poll without items", self.name))?;
        anyhow::ensure!(
            items.len() <= MAX_ITEMS,
            "{} sent more than {MAX_ITEMS} items",
            self.name
        );
        items
            .iter()
            .map(|i| {
                let item: PluginItem = serde_json::from_value(i.clone()).map_err(|e| {
                    anyhow::anyhow!("{} sent an item tab can't read: {e}", self.name)
                })?;
                item.found(kinds)
            })
            .collect()
    }

    pub async fn check(
        &self,
        s: &Session<'_>,
        item: &IncomingEvent,
    ) -> anyhow::Result<Option<bool>> {
        let r = self
            .request(
                s,
                "check",
                json!({ "external_id": item.external_id, "metadata": item.metadata }),
            )
            .await?;
        Ok(r["resolved"].as_bool())
    }

    /// Starts the program, asks once, answers its tool calls, and stops it.
    async fn request(&self, s: &Session<'_>, method: &str, params: Value) -> anyhow::Result<Value> {
        tokio::time::timeout(self.timeout, self.exchange(s, method, params))
            .await
            .map_err(|_| {
                anyhow::anyhow!(
                    "{} took longer than {}s and was stopped",
                    self.name,
                    self.timeout.as_secs()
                )
            })?
    }

    async fn exchange(
        &self,
        s: &Session<'_>,
        method: &str,
        params: Value,
    ) -> anyhow::Result<Value> {
        let exe = resolve(&self.spec.command, &self.dir)?;
        let sb = crate::sandbox::current();
        let network = if self.spec.network.is_empty() {
            Network::None
        } else {
            Network::Domains(self.spec.network.clone())
        };
        let policy = sb.isolated_policy(std::slice::from_ref(&self.dir), network);
        let args: Vec<std::ffi::OsString> = self.spec.args.iter().map(Into::into).collect();
        let mut prepared = sb.command(&exe, &args, &self.dir, &policy).await?;
        crate::sandbox::env::scrub(&mut prepared.cmd, crate::sandbox::Trust::Untrusted);
        prepared
            .cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = prepared
            .spawn()
            .map_err(|e| anyhow::anyhow!("couldn't start {}: {e}", self.spec.command))?;
        let mut stdin = child.stdin.take().expect("piped");
        let mut lines = BufReader::new(child.stdout.take().expect("piped")).lines();
        let mut stderr = child.stderr.take().expect("piped");
        let errors = tokio::spawn(async move {
            let mut s = String::new();
            let _ = stderr.read_to_string(&mut s).await;
            s
        });
        let send = |v: Value| format!("{v}\n");
        stdin
            .write_all(
                send(json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params }))
                    .as_bytes(),
            )
            .await?;
        stdin.flush().await?;
        let mut calls = 0;
        loop {
            let Some(line) = lines.next_line().await? else {
                drop(stdin);
                let _ = child.wait().await;
                let err = errors.await.unwrap_or_default();
                let tail: String = err
                    .trim()
                    .chars()
                    .rev()
                    .take(400)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                anyhow::bail!(
                    "{} stopped without answering{}",
                    self.name,
                    if tail.is_empty() {
                        String::new()
                    } else {
                        format!(": {tail}")
                    }
                );
            };
            if line.trim().is_empty() {
                continue;
            }
            let msg: Value = serde_json::from_str(&line).map_err(|e| {
                anyhow::anyhow!("{} wrote something that isn't JSON: {e}", self.name)
            })?;
            match (msg.get("method").and_then(Value::as_str), msg.get("id")) {
                // Its answer.
                (None, Some(id)) if id == &json!(1) => {
                    if let Some(e) = msg.get("error") {
                        anyhow::bail!(
                            "{}: {}",
                            self.name,
                            e["message"].as_str().unwrap_or("failed")
                        );
                    }
                    return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
                }
                // A request of its own: answer it.
                (Some(m), Some(id)) => {
                    let reply = match self.serve(s, m, &msg["params"], &mut calls).await {
                        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
                        Err(e) => json!({ "jsonrpc": "2.0", "id": id,
                            "error": { "code": -32000, "message": e.to_string() } }),
                    };
                    stdin.write_all(send(reply).as_bytes()).await?;
                    stdin.flush().await?;
                }
                (Some("log"), None) => {
                    tracing::info!(
                        plugin = self.name,
                        "{}",
                        msg["params"]["message"].as_str().unwrap_or_default()
                    );
                }
                _ => {}
            }
        }
    }

    async fn serve(
        &self,
        s: &Session<'_>,
        method: &str,
        params: &Value,
        calls: &mut usize,
    ) -> anyhow::Result<Value> {
        match method {
            "tools/list" => Ok(json!({ "tools": s.tools().iter().map(|t| json!({
                "name": t, "read_only": s.read_only(t),
            })).collect::<Vec<_>>() })),
            "tools/call" => {
                let name = params["name"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("tools/call needs a name"))?;
                anyhow::ensure!(s.has(name), "the server has no tool {name}");
                anyhow::ensure!(
                    self.built_in || s.read_only(name),
                    "{name} isn't marked read-only, so a plugin can't call it"
                );
                *calls += 1;
                anyhow::ensure!(*calls <= MAX_CALLS, "more than {MAX_CALLS} tool calls");
                s.call(name, params.get("arguments").cloned().unwrap_or(json!({})))
                    .await
            }
            other => anyhow::bail!("tab doesn't answer {other}"),
        }
    }
}

/// A program by name on PATH, or a path from the manifest's folder.
fn resolve(program: &str, dir: &Path) -> anyhow::Result<PathBuf> {
    let p = Path::new(program);
    if p.components().count() > 1 {
        let full = if p.is_absolute() {
            p.to_path_buf()
        } else {
            dir.join(p)
        };
        anyhow::ensure!(full.exists(), "{} doesn't exist", full.display());
        return Ok(full);
    }
    crate::lsp::find(program)
        .ok_or_else(|| anyhow::anyhow!("`{program}` isn't installed or isn't on PATH"))
}
