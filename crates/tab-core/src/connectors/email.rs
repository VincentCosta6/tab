//! Email over IMAP: MXroute, Fastmail, iCloud, or any IMAP host.
//!
//! Signs in over TLS, reads new mail from the folders you choose (the inbox by
//! default) and waits with IMAP IDLE, so new mail lands in seconds without
//! polling. Messages are read with `BODY.PEEK`, so nothing is marked read until
//! you mark the item done in tab.
//!
//! Spam and bulk mail are sorted out with rules, never with AI spend:
//! * Spam folders are never read. Your host's spam filter (MXroute runs one on
//!   every message) moves spam there before tab sees it.
//! * Anything the server still tagged as spam (`X-Spam-Flag`, `X-Spam-Status`,
//!   a spam score over your threshold) or that failed DMARC is skipped.
//! * Newsletters, lists and automated mail (`List-Unsubscribe`, `List-Id`,
//!   `Precedence: bulk`, `Auto-Submitted`, no-reply senders) come in as
//!   low-priority kinds marked rules-only, so the AI layer never reads them.
//!   Or skip them entirely in settings.

use std::sync::Arc;
use std::time::Duration;

use async_imap::Session;
use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use futures_util::TryStreamExt;
use mail_parser::{Address, HeaderValue, Message, MessageParser};
use serde_json::json;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use tokio_rustls::rustls;

use crate::config::ConnectorConfig;
use crate::connector::{Connector, ConnectorContext, ConnectorFactory};
use crate::gateway::Gateway;
use crate::model::{
    Block, ConnectorDescriptor, ConnectorStatus, EntityRef, Field, IncomingEvent, KindInfo,
    QueueItem, SettingField, SettingKind, Tone,
};

type Imap = Session<TlsStream<TcpStream>>;

const MAX_BODY: usize = 4000;
/// Bigger messages come in with headers only.
const MAX_FETCH_BYTES: u32 = 2 * 1024 * 1024;
/// How far back the first sync looks, and how much of it.
const FIRST_SYNC_DAYS: i64 = 7;
const FIRST_SYNC_MAX: usize = 100;

pub struct EmailFactory;

fn setting(key: &str, label: &str, kind: SettingKind, help: &str, required: bool) -> SettingField {
    SettingField {
        key: key.into(),
        label: label.into(),
        kind,
        help: (!help.is_empty()).then(|| help.into()),
        required,
    }
}

impl ConnectorFactory for EmailFactory {
    fn descriptor(&self) -> ConnectorDescriptor {
        ConnectorDescriptor {
            id: "email".into(),
            name: "Email (IMAP)".into(),
            icon: "mail".into(),
            description: "Mail from MXroute, Fastmail, iCloud or any IMAP host. Spam and newsletters are sorted with rules, not AI.".into(),
            kinds: vec![
                KindInfo::new("email.direct", "Email", "mail", Tone::Info),
                KindInfo::new("email.cc", "CC'd", "mail", Tone::Neutral),
                KindInfo::new("email.newsletter", "Newsletter", "mail", Tone::Neutral),
                KindInfo::new("email.automated", "Automated", "mail", Tone::Neutral),
            ],
            settings: vec![
                setting("host", "IMAP server", SettingKind::Text, "For MXroute, the server name from your welcome email, e.g. heracles.mxrouting.net.", true),
                setting("username", "Email address", SettingKind::Text, "", true),
                setting("password", "Password", SettingKind::Secret, "Your mailbox password, or an app password if your host uses them.", true),
                setting("port", "Port", SettingKind::Number, "993 (IMAP over TLS) unless your host says otherwise.", false),
                setting("folders", "Folders", SettingKind::Text, "Comma separated. Defaults to INBOX. Spam folders are never read.", false),
                setting("bulk", "Newsletters and automated mail", SettingKind::Text, "\"rules\" (default): import, rank with rules only. \"skip\": don't import. \"ai\": let the AI layer read them too.", false),
                setting("spam_score", "Spam score cutoff", SettingKind::Number, "Skip mail the server scored at or above this. Default 5.", false),
                setting("webmail_url", "Webmail link", SettingKind::Text, "Optional. Where Open takes you, e.g. your MXroute webmail.", false),
            ],
            available: true,
            rate_limits: Vec::new(),
            category: "email".into(),
            features: vec![
                "New mail arrives in seconds through IMAP IDLE".into(),
                "Your host's spam filter runs first; leftover spam and failed-DMARC mail are skipped".into(),
                "Newsletters and automated mail are ranked by rules only, so they never use AI".into(),
                "Marking an item done marks the email read".into(),
            ],
            builtin: false,
        }
    }

    fn build(
        &self,
        cfg: &ConnectorConfig,
        _gateway: Arc<Gateway>,
    ) -> Option<anyhow::Result<Arc<dyn Connector>>> {
        let settings = match Settings::from_config(cfg) {
            Ok(s) => s,
            Err(e) => return Some(Err(e)),
        };
        Some(Ok(Arc::new(EmailConnector { settings })))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BulkMode {
    Rules,
    Skip,
    Ai,
}

#[derive(Debug, Clone)]
pub struct Settings {
    host: String,
    port: u16,
    username: String,
    password: String,
    folders: Vec<String>,
    bulk: BulkMode,
    spam_score: f64,
    webmail_url: Option<String>,
}

impl Settings {
    fn from_config(cfg: &ConnectorConfig) -> anyhow::Result<Self> {
        let need = |k: &str, what: &str| {
            cfg.str(k)
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| anyhow::anyhow!("Add your {what} to connect email."))
        };
        let folders: Vec<String> = cfg
            .str("folders")
            .unwrap_or("INBOX")
            .split(',')
            .map(|f| f.trim().to_string())
            .filter(|f| !f.is_empty() && !is_spam_folder(f))
            .collect();
        Ok(Self {
            host: need("host", "IMAP server")?,
            port: cfg
                .u64("port")
                .and_then(|p| u16::try_from(p).ok())
                .unwrap_or(993),
            username: need("username", "email address")?,
            password: cfg
                .str("password")
                .map(str::to_string)
                .ok_or_else(|| anyhow::anyhow!("Add your password to connect email."))?,
            folders: if folders.is_empty() {
                vec!["INBOX".into()]
            } else {
                folders
            },
            bulk: match cfg
                .str("bulk")
                .map(|s| s.trim().to_ascii_lowercase())
                .as_deref()
            {
                Some("skip") => BulkMode::Skip,
                Some("ai") => BulkMode::Ai,
                _ => BulkMode::Rules,
            },
            spam_score: cfg
                .settings
                .get("spam_score")
                .and_then(|v| v.as_f64().or_else(|| v.as_str()?.parse().ok()))
                .unwrap_or(5.0),
            webmail_url: cfg.str("webmail_url").map(str::to_string),
        })
    }
}

fn is_spam_folder(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    ["spam", "junk", "bulk mail", "trash", "deleted"]
        .iter()
        .any(|s| n == *s || n.ends_with(&format!("/{s}")) || n.ends_with(&format!(".{s}")))
}

pub struct EmailConnector {
    settings: Settings,
}

async fn connect(s: &Settings) -> anyhow::Result<Imap> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    // A private CA, for self-hosted servers (and tests): TAB_IMAP_CA=/path/to/ca.pem
    if let Ok(path) = std::env::var("TAB_IMAP_CA") {
        use rustls::pki_types::{CertificateDer, pem::PemObject};
        for cert in CertificateDer::pem_file_iter(&path)? {
            roots.add(cert?)?;
        }
    }
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_root_certificates(roots)
    .with_no_client_auth();
    let tcp = tokio::time::timeout(
        Duration::from_secs(20),
        TcpStream::connect((s.host.as_str(), s.port)),
    )
    .await
    .map_err(|_| anyhow::anyhow!("{} didn't answer on port {}", s.host, s.port))??;
    let name = rustls::pki_types::ServerName::try_from(s.host.clone())?;
    let tls = TlsConnector::from(Arc::new(config))
        .connect(name, tcp)
        .await?;
    let mut client = async_imap::Client::new(tls);
    client
        .read_response()
        .await?
        .ok_or_else(|| anyhow::anyhow!("{} closed the connection", s.host))?;
    client
        .login(&s.username, &s.password)
        .await
        .map_err(|(e, _)| anyhow::anyhow!("Signing in to {} failed: {e}", s.host))
}

/// What one message turned into, or why it was left out.
#[derive(Debug, PartialEq)]
pub enum Sorted {
    Keep(Box<IncomingEvent>),
    Skip(&'static str),
}

fn header(m: &Message<'_>, name: &str) -> Option<String> {
    m.header_raw(name).map(|v| v.trim().to_string())
}

/// Spam score from the common headers (SpamAssassin, Rspamd, MXroute's filter).
fn spam_score(m: &Message<'_>) -> Option<f64> {
    if let Some(v) = header(m, "X-Spam-Score").or_else(|| header(m, "X-Rspamd-Score"))
        && let Ok(n) = v.split_whitespace().next().unwrap_or("").parse()
    {
        return Some(n);
    }
    let status = header(m, "X-Spam-Status").or_else(|| header(m, "X-Spamd-Result"))?;
    let i = status
        .find("score=")
        .map(|i| i + 6)
        .or_else(|| status.find('[').map(|i| i + 1))?;
    status[i..]
        .split(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-'))
        .next()?
        .parse()
        .ok()
}

fn is_spam(m: &Message<'_>, cutoff: f64) -> Option<&'static str> {
    let yes = |h: &str| header(m, h).is_some_and(|v| v.to_ascii_lowercase().starts_with("yes"));
    if yes("X-Spam-Flag") || yes("X-Spam-Status") || yes("X-Spam") {
        return Some("tagged as spam by the server");
    }
    if spam_score(m).is_some_and(|s| s >= cutoff) {
        return Some("spam score over the cutoff");
    }
    if header(m, "Authentication-Results")
        .is_some_and(|v| v.to_ascii_lowercase().contains("dmarc=fail"))
    {
        return Some("failed DMARC");
    }
    None
}

/// "newsletter", "automated", or None for mail a person sent.
fn bulk_kind(m: &Message<'_>, from: &str) -> Option<&'static str> {
    if !matches!(m.list_unsubscribe(), HeaderValue::Empty)
        || !matches!(m.list_id(), HeaderValue::Empty)
    {
        return Some("newsletter");
    }
    if let Some(p) = header(m, "Precedence") {
        let p = p.to_ascii_lowercase();
        if p == "bulk" || p == "list" || p == "junk" {
            return Some("newsletter");
        }
    }
    if header(m, "Auto-Submitted").is_some_and(|v| !v.eq_ignore_ascii_case("no")) {
        return Some("automated");
    }
    let local = from.split('@').next().unwrap_or("").to_ascii_lowercase();
    if [
        "noreply",
        "no-reply",
        "donotreply",
        "do-not-reply",
        "notifications",
        "mailer-daemon",
        "postmaster",
    ]
    .iter()
    .any(|n| local == *n || local.starts_with(&format!("{n}+")))
    {
        return Some("automated");
    }
    None
}

/// Turns one raw message into a queue event, or a reason to leave it out.
pub fn sort_message(
    raw: &[u8],
    uid: u32,
    folder: &str,
    received: Option<DateTime<Utc>>,
    s: &Settings,
) -> Sorted {
    let Some(m) = MessageParser::new().parse(raw) else {
        return Sorted::Skip("unreadable");
    };
    if let Some(why) = is_spam(&m, s.spam_score) {
        return Sorted::Skip(why);
    }
    let sender = m.from().and_then(|a| a.first());
    let from_addr = sender.and_then(|s| s.address()).unwrap_or("").to_string();
    let from_name = sender.and_then(|s| s.name()).map(str::to_string);
    let me = s.username.to_ascii_lowercase();
    let listed = |list: Option<&Address<'_>>| {
        list.is_some_and(|l| {
            l.iter()
                .any(|b| b.address().is_some_and(|a| a.eq_ignore_ascii_case(&me)))
        })
    };
    let bulk = bulk_kind(&m, &from_addr);
    if bulk.is_some() && s.bulk == BulkMode::Skip {
        return Sorted::Skip("newsletter or automated mail");
    }
    let kind = match bulk {
        Some(k) => k,
        None if listed(m.to()) => "direct",
        None if listed(m.cc()) => "cc",
        // Bcc, aliases and forwards land here: still a person writing to you.
        None => "direct",
    };
    let subject = m.subject().unwrap_or("(no subject)").trim().to_string();
    let mut body = m.body_text(0).map(|b| b.into_owned()).unwrap_or_default();
    // Drop quoted replies below the new text.
    if let Some(i) = body.find("\nOn ").filter(|i| {
        body[*i + 1..]
            .lines()
            .next()
            .is_some_and(|l| l.trim_end().ends_with("wrote:"))
    }) {
        body.truncate(i);
    }
    let body = body.trim().to_string();
    let short: String = body.chars().take(MAX_BODY).collect();
    let occurred = m
        .date()
        .and_then(|d| Utc.timestamp_opt(d.to_timestamp(), 0).single())
        .or(received)
        .unwrap_or_else(Utc::now);
    let message_id = m
        .message_id()
        .map(|id| id.trim_matches(['<', '>']).to_string())
        .unwrap_or_else(|| format!("{folder}:{uid}"));
    let actor = from_name
        .clone()
        .or_else(|| (!from_addr.is_empty()).then(|| from_addr.clone()));
    let rules_only = bulk.is_some() && s.bulk != BulkMode::Ai;

    let mut entities = Vec::new();
    if !from_addr.is_empty() {
        entities.push(EntityRef::new(
            "person",
            format!("email:{}", from_addr.to_ascii_lowercase()),
            actor.as_deref().unwrap_or(&from_addr),
            "from",
        ));
        if let Some(domain) = from_addr.split('@').nth(1) {
            entities.push(EntityRef::new(
                "org",
                format!("domain:{}", domain.to_ascii_lowercase()),
                domain,
                "from",
            ));
        }
    }
    let mut fields = vec![Field::new(
        "From",
        &match (&from_name, from_addr.is_empty()) {
            (Some(n), false) => format!("{n} <{from_addr}>"),
            (Some(n), true) => n.clone(),
            (None, _) => from_addr.clone(),
        },
    )];
    if folder != "INBOX" {
        fields.push(Field::new("Folder", folder));
    }
    let mut blocks = vec![Block::Fields { items: fields }];
    if !short.is_empty() {
        blocks.push(Block::Text {
            text: short.clone(),
            markdown: false,
        });
    }

    Sorted::Keep(Box::new(IncomingEvent {
        source: "email".into(),
        external_id: message_id.clone(),
        kind: format!("email.{kind}"),
        title: subject,
        body: (!short.is_empty()).then_some(short),
        url: s.webmail_url.clone(),
        actor,
        context: Some(if folder == "INBOX" {
            s.username.clone()
        } else {
            format!("{} / {folder}", s.username)
        }),
        occurred_at: occurred,
        blocks,
        entities,
        metadata: json!({
            "folder": folder,
            "uid": uid,
            "message_id": message_id,
            "from": from_addr,
            // The ranker reads this: newsletters and automated mail never use AI.
            "ai": !rules_only,
        }),
    }))
}

impl EmailConnector {
    /// Fetches every message in `folder` with a UID after `after` (or the last
    /// week on the first pass) and emits what survives the filters. Returns the
    /// highest UID seen.
    async fn sync_folder(
        &self,
        imap: &mut Imap,
        ctx: &ConnectorContext,
        folder: &str,
        after: Option<u32>,
    ) -> anyhow::Result<Option<u32>> {
        let mailbox = imap.examine(folder).await?;
        let uids: Vec<u32> = match after {
            Some(last) => {
                let mut v: Vec<u32> = imap
                    .uid_search(format!("UID {}:*", last + 1))
                    .await?
                    .into_iter()
                    .filter(|u| *u > last)
                    .collect();
                v.sort_unstable();
                v
            }
            None => {
                let since =
                    (Utc::now() - chrono::Duration::days(FIRST_SYNC_DAYS)).format("%d-%b-%Y");
                let mut v: Vec<u32> = imap
                    .uid_search(format!("SINCE {since}"))
                    .await?
                    .into_iter()
                    .collect();
                v.sort_unstable();
                let skip = v.len().saturating_sub(FIRST_SYNC_MAX);
                v.split_off(skip)
            }
        };
        let mut highest = after.or_else(|| mailbox.uid_next.map(|n| n.saturating_sub(1)));
        if uids.is_empty() {
            return Ok(highest);
        }
        let set = uids
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(",");
        // Sizes first, so huge messages come in as headers only.
        let sizes: Vec<(u32, u32, Option<DateTime<Utc>>)> = imap
            .uid_fetch(&set, "(UID RFC822.SIZE INTERNALDATE)")
            .await?
            .try_filter_map(|f| async move {
                Ok(f.uid.map(|u| {
                    (
                        u,
                        f.size.unwrap_or(0),
                        f.internal_date().map(|d| d.with_timezone(&Utc)),
                    )
                }))
            })
            .try_collect()
            .await?;
        let (mut kept, mut skipped) = (0, 0);
        for (uid, size, received) in sizes {
            let query = if size > MAX_FETCH_BYTES {
                "(UID BODY.PEEK[HEADER])"
            } else {
                "(UID BODY.PEEK[])"
            };
            let fetched: Vec<async_imap::types::Fetch> = imap
                .uid_fetch(uid.to_string(), query)
                .await?
                .try_collect()
                .await?;
            let raw = fetched
                .iter()
                .find_map(|f| f.body().or(f.header()).map(<[u8]>::to_vec));
            highest = Some(highest.map_or(uid, |h| h.max(uid)));
            let Some(raw) = raw else { continue };
            match sort_message(&raw, uid, folder, received, &self.settings) {
                Sorted::Keep(ev) => {
                    kept += 1;
                    ctx.emit(*ev).await?;
                }
                Sorted::Skip(why) => {
                    skipped += 1;
                    tracing::debug!(uid, folder, why, "email left out");
                }
            }
        }
        tracing::info!(folder, kept, skipped, "email synced");
        Ok(highest)
    }

    async fn session(
        &self,
        ctx: &ConnectorContext,
        last: &mut [Option<u32>],
    ) -> anyhow::Result<()> {
        let mut imap = connect(&self.settings).await?;
        let idle_ok = imap.capabilities().await?.has_str("IDLE");
        loop {
            for (i, folder) in self.settings.folders.iter().enumerate() {
                last[i] = self.sync_folder(&mut imap, ctx, folder, last[i]).await?;
            }
            ctx.synced().await;
            // IDLE watches one folder; with several, poll every minute instead.
            if idle_ok && self.settings.folders.len() == 1 {
                imap.select(&self.settings.folders[0]).await?;
                let mut idle = imap.idle();
                idle.init().await?;
                let (wait, stop) = idle.wait_with_timeout(Duration::from_secs(25 * 60));
                tokio::select! {
                    r = wait => { r?; }
                    _ = ctx.cancelled() => { drop(stop); return Ok(()); }
                }
                imap = idle.done().await?;
            } else if !ctx.sleep(Duration::from_secs(60)).await {
                let _ = imap.logout().await;
                return Ok(());
            }
        }
    }
}

#[async_trait]
impl Connector for EmailConnector {
    fn id(&self) -> &str {
        "email"
    }

    async fn run(&self, ctx: ConnectorContext) -> anyhow::Result<()> {
        let mut last: Vec<Option<u32>> = vec![None; self.settings.folders.len()];
        let mut backoff = Duration::from_secs(10);
        while !ctx.is_cancelled() {
            match self.session(&ctx, &mut last).await {
                Ok(()) => return Ok(()),
                Err(e) => {
                    tracing::warn!(error = %e, "email connection dropped");
                    ctx.set_status(ConnectorStatus::Error {
                        message: format!("{e:#}"),
                    })
                    .await;
                    if !ctx.sleep(backoff).await {
                        return Ok(());
                    }
                    backoff = (backoff * 2).min(Duration::from_secs(300));
                }
            }
        }
        Ok(())
    }

    /// Done in tab marks the email read.
    async fn on_done(&self, item: &QueueItem) -> anyhow::Result<()> {
        let meta = &item.event.metadata;
        let (Some(folder), Some(uid)) = (meta["folder"].as_str(), meta["uid"].as_u64()) else {
            return Ok(());
        };
        let mut imap = connect(&self.settings).await?;
        imap.select(folder).await?;
        imap.uid_store(uid.to_string(), "+FLAGS (\\Seen)")
            .await?
            .try_collect::<Vec<_>>()
            .await?;
        let _ = imap.logout().await;
        Ok(())
    }

    /// Everything is captured at ingest; there's nothing to refetch.
    async fn preview(&self, item: &QueueItem) -> anyhow::Result<Vec<Block>> {
        Ok(item.event.blocks.clone())
    }

    fn preview_ttl(&self) -> Duration {
        Duration::from_secs(365 * 24 * 3600)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(bulk: BulkMode) -> Settings {
        Settings {
            host: "mail.example.com".into(),
            port: 993,
            username: "vince@example.com".into(),
            password: "x".into(),
            folders: vec!["INBOX".into()],
            bulk,
            spam_score: 5.0,
            webmail_url: None,
        }
    }

    fn msg(extra: &str, from: &str, to: &str) -> Vec<u8> {
        format!(
            "From: {from}\r\nTo: {to}\r\nSubject: Hello\r\nMessage-ID: <abc@x>\r\nDate: Wed, 4 Nov 2026 10:00:00 +0000\r\n{extra}\r\nCan you look at this today?\r\n\r\nOn Tue someone wrote:\r\n> old\r\n"
        )
        .into_bytes()
    }

    fn kind(s: Sorted) -> String {
        match s {
            Sorted::Keep(e) => format!("{} ai={}", e.kind, e.metadata["ai"]),
            Sorted::Skip(why) => format!("skip: {why}"),
        }
    }

    #[test]
    fn sorts_people_bulk_and_spam_without_ai() {
        let s = settings(BulkMode::Rules);
        let person = "Ann <ann@acme.com>";
        let me = "vince@example.com";
        assert_eq!(
            kind(sort_message(&msg("", person, me), 1, "INBOX", None, &s)),
            "email.direct ai=true"
        );
        assert_eq!(
            kind(sort_message(
                &msg("", person, "team@acme.com"),
                1,
                "INBOX",
                None,
                &s
            )),
            "email.direct ai=true"
        );
        assert_eq!(
            kind(sort_message(
                &msg(
                    "List-Unsubscribe: <https://x/u>\r\n",
                    "News <news@x.com>",
                    me
                ),
                1,
                "INBOX",
                None,
                &s
            )),
            "email.newsletter ai=false"
        );
        assert_eq!(
            kind(sort_message(
                &msg("", "GitHub <noreply@github.com>", me),
                1,
                "INBOX",
                None,
                &s
            )),
            "email.automated ai=false"
        );
        assert_eq!(
            kind(sort_message(
                &msg("X-Spam-Status: Yes, score=9.1\r\n", person, me),
                1,
                "INBOX",
                None,
                &s
            )),
            "skip: tagged as spam by the server"
        );
        assert_eq!(
            kind(sort_message(
                &msg("X-Spam-Status: No, score=6.2 required=8\r\n", person, me),
                1,
                "INBOX",
                None,
                &s
            )),
            "skip: spam score over the cutoff"
        );
        assert_eq!(
            kind(sort_message(
                &msg(
                    "Authentication-Results: mx; dmarc=fail (p=reject)\r\n",
                    person,
                    me
                ),
                1,
                "INBOX",
                None,
                &s
            )),
            "skip: failed DMARC"
        );
        assert_eq!(
            kind(sort_message(
                &msg("Precedence: bulk\r\n", person, me),
                1,
                "INBOX",
                None,
                &settings(BulkMode::Skip)
            )),
            "skip: newsletter or automated mail"
        );
        // Quoted reply is trimmed.
        let Sorted::Keep(e) = sort_message(&msg("", person, me), 1, "INBOX", None, &s) else {
            panic!()
        };
        assert_eq!(e.body.as_deref(), Some("Can you look at this today?"));
        assert_eq!(e.external_id, "abc@x");
        assert!(is_spam_folder("Junk") && is_spam_folder("INBOX.spam") && !is_spam_folder("INBOX"));
    }
}
