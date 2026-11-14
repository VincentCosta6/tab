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
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use tokio_rustls::rustls;

use crate::config::ConnectorConfig;
use crate::connector::{Connector, ConnectorContext, ConnectorFactory};
use crate::gateway::Gateway;
use crate::model::{
    ActionField, ActionInfo, Block, ConnectorDescriptor, ConnectorStatus, Effect, EntityRef, Field,
    FieldType, IncomingEvent, KindInfo, QueueItem, SettingField, SettingKind, Tone,
};

type Imap = Session<TlsStream<crate::net::Stream>>;

const MAX_BODY: usize = 4000;
/// Bigger messages come in with headers only.
const MAX_FETCH_BYTES: u32 = 2 * 1024 * 1024;
/// How far back the first sync looks, and how much of it.
const FIRST_SYNC_DAYS: i64 = 7;
const FIRST_SYNC_MAX: usize = 100;

pub struct EmailFactory;

fn message_fields() -> Vec<ActionField> {
    vec![
        ActionField::new("folder", "Folder", FieldType::Text)
            .required()
            .default("{{item.metadata.folder}}"),
        ActionField::new("uid", "Message", FieldType::Number)
            .required()
            .help("The message's IMAP UID. Filled in from the item.")
            .default("{{item.metadata.uid}}"),
    ]
}

/// What workflows can do with mail. Each is one short IMAP session.
pub fn actions() -> Vec<ActionInfo> {
    vec![
        ActionInfo::new(
            "message.mark_read",
            "Mark read",
            "check",
            Effect::Write,
            "Marks the email read on the server.",
        )
        .inputs(message_fields())
        .for_kinds(&["email.*"]),
        ActionInfo::new(
            "message.flag",
            "Flag",
            "pin",
            Effect::Write,
            "Flags (stars) the email so it stands out in your mail app.",
        )
        .inputs(message_fields())
        .for_kinds(&["email.*"]),
        ActionInfo::new(
            "message.move",
            "Move to folder",
            "folder",
            Effect::Destructive,
            "Moves the email to another folder, such as Archive.",
        )
        .inputs({
            let mut f = message_fields();
            f.push(
                ActionField::new("to", "Move to", FieldType::Text)
                    .required()
                    .default("Archive"),
            );
            f
        })
        .for_kinds(&["email.*"]),
    ]
}

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
                setting("password", "Password", SettingKind::Secret, "Your mailbox password, or an app password if your host uses them. Not needed when you sign in with Google or Microsoft.", true),
                setting("port", "Port", SettingKind::Number, "993 (IMAP over TLS) unless your host says otherwise.", false),
                setting("folders", "Folders", SettingKind::Text, "Comma separated. Defaults to INBOX. Spam folders are never read.", false),
                setting("bulk", "Newsletters and automated mail", SettingKind::Text, "\"rules\" (default): import, rank with rules only. \"skip\": don't import. \"ai\": let the AI layer read them too.", false),
                setting("spam_score", "Spam score cutoff", SettingKind::Number, "Skip mail the server scored at or above this. Default 5.", false),
                setting("webmail_url", "Webmail link", SettingKind::Text, "Optional. Where Open takes you. MXroute, Gmail, Fastmail and iCloud are filled in for you; a Roundcube link opens the message itself.", false),
            ],
            available: true,
            rate_limits: Vec::new(),
            category: "communication".into(),
            features: vec![
                "New mail arrives in seconds through IMAP IDLE".into(),
                "Your host's spam filter runs first; leftover spam and failed-DMARC mail are skipped".into(),
                "Newsletters and automated mail are ranked by rules only, so they never use AI".into(),
                "Marking an item done marks the email read".into(),
            ],
            builtin: false,
            actions: actions(),
            clis: Vec::new(),
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

/// How the connection signs in to IMAP.
#[derive(Debug, Clone)]
enum Auth {
    Password(String),
    /// "Sign in with Google / Microsoft" ([`crate::mail_oauth`]): XOAUTH2
    /// with a token from the vault. Set by the sign-in as `auth`.
    OAuth(crate::mail_oauth::Provider),
}

#[derive(Debug, Clone)]
pub struct Settings {
    host: String,
    port: u16,
    username: String,
    auth: Auth,
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
            auth: match cfg.str("auth").and_then(crate::mail_oauth::Provider::parse) {
                Some(p) => Auth::OAuth(p),
                None => Auth::Password(
                    cfg.str("password")
                        .map(str::to_string)
                        .ok_or_else(|| anyhow::anyhow!("Add your password to connect email."))?,
                ),
            },
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

fn tls_config() -> anyhow::Result<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    // A private CA, for self-hosted servers (and tests): TAB_IMAP_CA=/path/to/ca.pem
    if let Ok(path) = std::env::var("TAB_IMAP_CA") {
        use rustls::pki_types::{CertificateDer, pem::PemObject};
        for cert in CertificateDer::pem_file_iter(&path)? {
            roots.add(cert?)?;
        }
    }
    Ok(rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_root_certificates(roots)
    .with_no_client_auth())
}

/// Opens IMAP over TLS and reads the server's greeting, without signing in.
async fn open(
    host: &str,
    port: u16,
    wait: Duration,
) -> anyhow::Result<async_imap::Client<TlsStream<crate::net::Stream>>> {
    // Through the company proxy when one is set (see crate::net).
    let tcp = tokio::time::timeout(wait, crate::net::connect(host, port, true))
        .await
        .map_err(|_| anyhow::anyhow!("{host} didn't answer on port {port}"))??;
    let name = rustls::pki_types::ServerName::try_from(host.to_string())?;
    let tls = tokio::time::timeout(
        wait,
        TlsConnector::from(Arc::new(tls_config()?)).connect(name, tcp),
    )
    .await
    .map_err(|_| anyhow::anyhow!("{host} didn't finish TLS on port {port}"))?
    .map_err(|e| anyhow::anyhow!("TLS with {host} failed: {e}"))?;
    let mut client = async_imap::Client::new(tls);
    tokio::time::timeout(wait, client.read_response())
        .await
        .map_err(|_| anyhow::anyhow!("{host} sent no IMAP greeting"))??
        .ok_or_else(|| anyhow::anyhow!("{host} closed the connection"))?;
    Ok(client)
}

async fn connect(s: &Settings) -> anyhow::Result<Imap> {
    let client = open(&s.host, s.port, Duration::from_secs(20)).await?;
    match &s.auth {
        Auth::Password(password) => client.login(&s.username, password).await,
        Auth::OAuth(p) => {
            let (user, token) = crate::mail_oauth::access_token(*p).await?;
            client
                .authenticate("XOAUTH2", crate::mail_oauth::XOAuth2 { user, token })
                .await
        }
    }
    .map_err(|(e, _)| anyhow::anyhow!("Signing in to {} failed: {e}", s.host))
}

/// Signs in with saved settings and reports what tab will read, for the
/// setup assistant. Never returns the password.
pub async fn check(cfg: &ConnectorConfig) -> anyhow::Result<String> {
    let s = Settings::from_config(cfg)?;
    let mut imap = connect(&s).await?;
    let names: Vec<String> = imap
        .list(Some(""), Some("*"))
        .await?
        .try_collect::<Vec<_>>()
        .await?
        .iter()
        .map(|n| n.name().to_string())
        .collect();
    let mut out = format!(
        "Signed in to {}:{} as {}.\nFolders on the server: {}\n",
        s.host,
        s.port,
        s.username,
        names.join(", ")
    );
    for f in &s.folders {
        match imap.examine(f).await {
            Ok(m) => out.push_str(&format!("tab reads {f}: {} messages.\n", m.exists)),
            Err(e) => out.push_str(&format!("Can't open folder {f}: {e}\n")),
        }
    }
    let skipped: Vec<_> = names.iter().filter(|n| is_spam_folder(n)).collect();
    if !skipped.is_empty() {
        out.push_str(&format!(
            "Never read (spam and trash): {}\n",
            skipped
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let _ = imap.logout().await;
    Ok(out)
}

/// An IMAP server that might serve an address.
#[derive(Debug, Clone, serde::Serialize, PartialEq)]
pub struct ImapCandidate {
    pub host: String,
    pub port: u16,
    /// Where the guess came from: provider, ISPDB, MX, or a common name.
    pub source: String,
    /// It answered with a valid certificate and an IMAP greeting.
    pub reachable: bool,
    pub note: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Discovery {
    pub domain: String,
    pub mx: Vec<String>,
    pub provider: Option<String>,
    /// Provider-specific advice, e.g. "needs an app password".
    pub advice: Option<String>,
    /// Best first: known provider settings, then servers that answered.
    pub candidates: Vec<ImapCandidate>,
}

/// Mail hosts tab knows by their MX records.
fn known_provider(
    mx: &[String],
    domain: &str,
) -> Option<(&'static str, Option<String>, &'static str)> {
    let any =
        |pat: &str| mx.iter().any(|m| m.ends_with(pat)) || domain == pat.trim_start_matches('.');
    if let Some(m) = mx
        .iter()
        .find(|m| m.ends_with(".mxrouting.net") || m.ends_with(".mxlogin.com"))
    {
        return Some((
            "MXroute",
            Some(m.clone()),
            "MXroute's IMAP server is the server name from your welcome email, which matches your MX record. Use your full email address and mailbox password.",
        ));
    }
    if any(".google.com") || any(".googlemail.com") || domain == "gmail.com" {
        return Some((
            "Google",
            Some("imap.gmail.com".into()),
            if crate::mail_oauth::Provider::Google.available() {
                "Gmail: ask the person to press \"Sign in with Google\" in Connections → Email; no password needed. Otherwise it needs IMAP on and an app password (Google Account → Security → App passwords)."
            } else {
                "Gmail needs IMAP turned on and an app password (Google Account → Security → App passwords). Your normal password won't work."
            },
        ));
    }
    if any(".outlook.com")
        || any(".hotmail.com")
        || domain == "outlook.com"
        || domain == "hotmail.com"
    {
        return Some((
            "Microsoft",
            Some("outlook.office365.com".into()),
            if crate::mail_oauth::Provider::Microsoft.available() {
                "Microsoft turned off password sign-in for IMAP: ask the person to press \"Sign in with Microsoft\" in Connections → Email instead of typing a password."
            } else {
                "Microsoft turned off password sign-in for IMAP on most accounts, so this may fail. This build of tab can't sign in with Microsoft yet."
            },
        ));
    }
    if any(".icloud.com") || domain == "icloud.com" || domain == "me.com" {
        return Some((
            "iCloud",
            Some("imap.mail.me.com".into()),
            "iCloud needs an app-specific password (account.apple.com → Sign-In and Security).",
        ));
    }
    if any(".messagingengine.com") || domain == "fastmail.com" {
        return Some((
            "Fastmail",
            Some("imap.fastmail.com".into()),
            "Fastmail needs an app password (Settings → Privacy & Security → App passwords).",
        ));
    }
    if any(".yahoodns.net") || domain == "yahoo.com" {
        return Some((
            "Yahoo",
            Some("imap.mail.yahoo.com".into()),
            "Yahoo needs an app password (Account security → Generate app password).",
        ));
    }
    None
}

/// MX hosts from a DNS-over-HTTPS JSON answer, lowest preference first.
fn parse_mx(v: &serde_json::Value) -> Vec<String> {
    let mut mx: Vec<(u32, String)> = v["Answer"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|a| a["type"].as_u64() == Some(15))
        .filter_map(|a| {
            let (pref, host) = a["data"].as_str()?.split_once(' ')?;
            Some((
                pref.parse().ok()?,
                host.trim_end_matches('.').to_ascii_lowercase(),
            ))
        })
        .collect();
    mx.sort();
    mx.into_iter().map(|(_, h)| h).collect()
}

/// IMAP servers from a Thunderbird autoconfig (ISPDB) document.
fn parse_autoconfig(xml: &str, address: &str, domain: &str) -> Vec<(String, u16)> {
    let tag = |block: &str, name: &str| -> Option<String> {
        let open = format!("<{name}>");
        let start = block.find(&open)? + open.len();
        let end = block[start..].find(&format!("</{name}>"))? + start;
        Some(block[start..end].trim().to_string())
    };
    xml.split("<incomingServer")
        .skip(1)
        .filter(|b| b.starts_with(" type=\"imap\""))
        .filter(|b| tag(b, "socketType").as_deref() == Some("SSL"))
        .filter_map(|b| {
            let host = tag(b, "hostname")?
                .replace("%EMAILDOMAIN%", domain)
                .replace("%EMAILADDRESS%", address);
            Some((host, tag(b, "port")?.parse().ok()?))
        })
        .collect()
}

/// Works out which IMAP server serves an address: known providers from the
/// MX records, Thunderbird's provider database, then common host names, each
/// checked live for a TLS certificate and an IMAP greeting.
pub async fn discover(address: &str) -> anyhow::Result<Discovery> {
    let address = address.trim();
    let domain = address
        .rsplit_once('@')
        .map(|(_, d)| d)
        .unwrap_or(address)
        .trim()
        .to_ascii_lowercase();
    anyhow::ensure!(
        domain.contains('.'),
        "{address} doesn't look like an email address"
    );
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .build()?;

    let mx = match http
        .get("https://cloudflare-dns.com/dns-query")
        .query(&[("name", domain.as_str()), ("type", "MX")])
        .header("accept", "application/dns-json")
        .send()
        .await
    {
        Ok(r) => parse_mx(&r.json().await.unwrap_or_default()),
        Err(_) => Vec::new(),
    };

    let mut guesses: Vec<(String, u16, String)> = Vec::new();
    let known = known_provider(&mx, &domain);
    if let Some((name, Some(host), _)) = &known {
        guesses.push((host.clone(), 993, format!("{name} (known provider)")));
    }
    // Thunderbird's provider database, by domain and then by the MX's domain.
    let mut lookups = vec![domain.clone()];
    if let Some(m) = mx.first() {
        let parts: Vec<_> = m.split('.').collect();
        if parts.len() >= 2 {
            lookups.push(parts[parts.len() - 2..].join("."));
        }
    }
    for d in lookups {
        if let Ok(r) = http
            .get(format!("https://autoconfig.thunderbird.net/v1.1/{d}"))
            .send()
            .await
            && r.status().is_success()
            && let Ok(xml) = r.text().await
        {
            for (h, p) in parse_autoconfig(&xml, address, &domain) {
                guesses.push((h, p, "Thunderbird provider database".into()));
            }
            break;
        }
    }
    for m in mx.iter().take(2) {
        guesses.push((m.clone(), 993, "MX record".into()));
    }
    for prefix in ["imap", "mail"] {
        guesses.push((format!("{prefix}.{domain}"), 993, "common name".into()));
    }
    let mut seen = std::collections::HashSet::new();
    guesses.retain(|(h, p, _)| seen.insert((h.clone(), *p)));
    guesses.truncate(8);

    let probes = guesses.into_iter().map(|(host, port, source)| async move {
        let r = open(&host, port, Duration::from_secs(6)).await;
        ImapCandidate {
            reachable: r.is_ok(),
            note: r.err().map(|e| e.to_string()),
            host,
            port,
            source,
        }
    });
    let mut candidates = futures_util::future::join_all(probes).await;
    // Keep the order of confidence, but servers that answered go first.
    candidates.sort_by_key(|c| !c.reachable);
    Ok(Discovery {
        domain,
        mx,
        provider: known.as_ref().map(|k| k.0.to_string()),
        advice: known.map(|k| k.2.to_string()),
        candidates,
    })
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
    let mut body = body_text(&m);
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
        url: open_url(s.webmail_url.as_deref(), &s.host, folder, uid, &message_id),
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

/// Largest formatted body rendered; bigger ones show as text.
const MAX_HTML: usize = 1024 * 1024;

fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The full message for the detail pane: who it's between, the formatted
/// body (with inline images embedded), and attachment names.
pub fn render_message(raw: &[u8]) -> Vec<Block> {
    use mail_parser::MimeHeaders;
    let Some(m) = MessageParser::new().parse(raw) else {
        return vec![Block::Text {
            text: "This email couldn't be read.".into(),
            markdown: false,
        }];
    };
    let people = |a: Option<&Address<'_>>| {
        a.map(|a| {
            a.iter()
                .map(|p| match (p.name(), p.address()) {
                    (Some(n), Some(e)) => format!("{n} <{e}>"),
                    (Some(n), None) => n.to_string(),
                    (None, Some(e)) => e.to_string(),
                    _ => String::new(),
                })
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .filter(|s| !s.is_empty())
    };
    let mut fields = Vec::new();
    for (label, v) in [
        ("From", people(m.from())),
        ("To", people(m.to())),
        ("Cc", people(m.cc())),
    ] {
        if let Some(v) = v {
            fields.push(Field::new(label, v));
        }
    }
    if let Some(d) = m.date() {
        fields.push(Field::new("Date", d.to_rfc822()));
    }
    let mut blocks = vec![Block::Fields { items: fields }];

    let has_html = m.html_bodies().any(|p| p.is_text_html());
    match m.body_html(0).filter(|h| has_html && h.len() <= MAX_HTML) {
        Some(html) => {
            let mut html = html.into_owned();
            // Inline images (cid:) become data URLs so they show without the network.
            for a in m.attachments() {
                let (Some(cid), Some(ct)) = (a.content_id(), a.content_type()) else {
                    continue;
                };
                if ct.ctype() != "image" || a.contents().len() > 2 * 1024 * 1024 {
                    continue;
                }
                let cid = cid.trim_matches(['<', '>']);
                let data = format!(
                    "data:image/{};base64,{}",
                    ct.subtype().unwrap_or("png"),
                    base64(a.contents())
                );
                html = html.replace(&format!("cid:{cid}"), &data);
            }
            blocks.push(Block::Html { html });
        }
        None => {
            let text = m.body_text(0).map(|t| t.into_owned()).unwrap_or_default();
            if !text.trim().is_empty() {
                blocks.push(Block::Text {
                    text: text.chars().take(100_000).collect(),
                    markdown: false,
                });
            }
        }
    }
    let files: Vec<_> = m
        .attachments()
        .filter(|a| a.content_id().is_none() || a.attachment_name().is_some())
        .filter_map(|a| a.attachment_name().map(|n| Field::new("attachment", n)))
        .collect();
    if !files.is_empty() {
        blocks.push(Block::Badges { items: files });
    }
    blocks
}

/// Where Open goes for one message. Roundcube (MXroute's webmail) and Gmail
/// open the message itself; other webmail opens the inbox.
pub fn open_url(
    webmail: Option<&str>,
    host: &str,
    folder: &str,
    uid: u32,
    message_id: &str,
) -> Option<String> {
    let enc = |s: &str| -> String {
        s.bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    (b as char).to_string()
                }
                _ => format!("%{b:02X}"),
            })
            .collect()
    };
    let base = match webmail.map(str::trim).filter(|w| !w.is_empty()) {
        Some(w) if w.contains("://") => w.to_string(),
        Some(w) => format!("https://{w}"),
        None if host.ends_with(".mxrouting.net") || host.ends_with(".mxlogin.com") => {
            format!("https://{host}/roundcube/")
        }
        None if host == "imap.gmail.com" => "https://mail.google.com/".into(),
        None if host == "imap.fastmail.com" => "https://app.fastmail.com/".into(),
        None if host == "imap.mail.me.com" => "https://www.icloud.com/mail/".into(),
        None => return None,
    };
    if base.contains("mail.google.com") {
        return Some(format!(
            "https://mail.google.com/mail/u/0/#search/rfc822msgid%3A{}",
            enc(message_id)
        ));
    }
    if base.contains("roundcube") {
        let sep = if base.contains('?') { '&' } else { '?' };
        return Some(format!(
            "{base}{sep}_task=mail&_mbox={}&_uid={uid}&_action=show",
            enc(folder)
        ));
    }
    Some(base)
}

/// How far back a project's email resource reads, and how many messages.
const SELECTION_DAYS: i64 = 365;

/// The messages an email resource stands for, as documents for the
/// project's index: `label:<name>` reads that folder (Gmail labels are
/// folders over IMAP), `from:<address>` that sender's mail in the folders
/// the connection reads. Newest first, at most `limit`.
pub async fn read_selection(
    cfg: &ConnectorConfig,
    selector: &str,
    limit: usize,
) -> anyhow::Result<Vec<crate::knowledge::chunk::Doc>> {
    let s = Settings::from_config(cfg)?;
    let (folders, query) = if let Some(label) = selector.strip_prefix("label:") {
        let label = label.trim();
        anyhow::ensure!(!label.is_empty(), "which label?");
        (vec![label.to_string()], String::new())
    } else if let Some(from) = selector.strip_prefix("from:") {
        let from = from.trim();
        anyhow::ensure!(
            !from.is_empty() && !from.contains(['"', '\\', '\r', '\n']),
            "that isn't an email address"
        );
        (s.folders.clone(), format!(" FROM \"{from}\""))
    } else {
        anyhow::bail!("an email resource is label:<name> or from:<address>, not {selector:?}");
    };
    let since = (Utc::now() - chrono::Duration::days(SELECTION_DAYS)).format("%d-%b-%Y");
    let mut imap = connect(&s).await?;
    let mut docs = Vec::new();
    for folder in &folders {
        if let Err(e) = imap.examine(folder).await {
            anyhow::bail!("couldn't open {folder}: {e}");
        }
        let mut uids: Vec<u32> = imap
            .uid_search(format!("SINCE {since}{query}"))
            .await?
            .into_iter()
            .collect();
        uids.sort_unstable();
        let uids = &uids[uids.len().saturating_sub(limit)..];
        if uids.is_empty() {
            continue;
        }
        let set = uids
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let small: Vec<u32> = imap
            .uid_fetch(&set, "(UID RFC822.SIZE)")
            .await?
            .try_filter_map(|f| async move {
                Ok(f.uid.filter(|_| f.size.unwrap_or(0) <= MAX_FETCH_BYTES))
            })
            .try_collect()
            .await?;
        for uid in small.into_iter().rev() {
            let fetched: Vec<async_imap::types::Fetch> = imap
                .uid_fetch(uid.to_string(), "(UID BODY.PEEK[])")
                .await?
                .try_collect()
                .await?;
            let Some(raw) = fetched.iter().find_map(|f| f.body()) else {
                continue;
            };
            let Some(m) = MessageParser::new().parse(raw) else {
                continue;
            };
            let subject = m.subject().unwrap_or("(no subject)").to_string();
            let sender = m
                .from()
                .and_then(|a| a.first())
                .map(|p| match (p.name(), p.address()) {
                    (Some(n), Some(e)) => format!("{n} <{e}>"),
                    (n, e) => n.or(e).unwrap_or_default().to_string(),
                })
                .unwrap_or_default();
            let date = m.date().map(|d| d.to_rfc3339()).unwrap_or_default();
            let body = body_text(&m);
            let mid = m.message_id().unwrap_or_default().to_string();
            docs.push(crate::knowledge::chunk::Doc {
                title: subject.clone(),
                source: open_url(s.webmail_url.as_deref(), &s.host, folder, uid, &mid)
                    .or_else(|| (!mid.is_empty()).then(|| format!("mid:{mid}"))),
                text: format!(
                    "From: {sender}\nDate: {date}\nSubject: {subject}\n\n{}",
                    body.trim()
                ),
            });
        }
    }
    let _ = imap.logout().await;
    docs.truncate(limit);
    Ok(docs)
}

impl EmailConnector {
    async fn fetch_raw(&self, folder: &str, uid: u64) -> anyhow::Result<Option<Vec<u8>>> {
        let mut imap = connect(&self.settings).await?;
        imap.examine(folder).await?;
        let msgs: Vec<_> = imap
            .uid_fetch(uid.to_string(), "BODY.PEEK[]")
            .await?
            .try_collect()
            .await?;
        let raw = msgs.iter().find_map(|f| f.body().map(<[u8]>::to_vec));
        let _ = imap.logout().await;
        Ok(raw)
    }

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
                    // Unread only: mail you finished (marked done here, or read
                    // elsewhere) doesn't come back after a restart.
                    .uid_search(format!("UNSEEN SINCE {since}"))
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

    async fn act(
        &self,
        action: &str,
        input: &serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        let folder = input["folder"].as_str().unwrap_or_default().trim();
        anyhow::ensure!(!folder.is_empty(), "folder is required");
        let uid = input["uid"]
            .as_u64()
            .or_else(|| input["uid"].as_str().and_then(|s| s.trim().parse().ok()))
            .ok_or_else(|| anyhow::anyhow!("uid must be a message UID"))?;
        let mut imap = connect(&self.settings).await?;
        imap.select(folder).await?;
        let result = match action {
            "message.mark_read" | "message.flag" => {
                let flag = if action == "message.flag" {
                    "\\Flagged"
                } else {
                    "\\Seen"
                };
                imap.uid_store(uid.to_string(), format!("+FLAGS ({flag})"))
                    .await?
                    .try_collect::<Vec<_>>()
                    .await?;
                json!({})
            }
            "message.move" => {
                let to = input["to"].as_str().unwrap_or_default().trim();
                anyhow::ensure!(!to.is_empty(), "say which folder to move it to");
                imap.uid_mv(uid.to_string(), to).await?;
                json!({ "folder": to })
            }
            other => anyhow::bail!("email has no action {other}"),
        };
        let _ = imap.logout().await;
        Ok(result)
    }

    /// The whole message, formatted, fetched once from the server and cached.
    /// Falls back to what was captured at ingest.
    async fn preview(&self, item: &QueueItem) -> anyhow::Result<Vec<Block>> {
        let m = &item.event.metadata;
        let (Some(folder), Some(uid)) = (m["folder"].as_str(), m["uid"].as_u64()) else {
            return Ok(item.event.blocks.clone());
        };
        match self.fetch_raw(folder, uid).await {
            Ok(Some(raw)) => Ok(render_message(&raw)),
            Ok(None) => Ok(item.event.blocks.clone()),
            Err(e) => {
                tracing::warn!(error = %e, "couldn't fetch the full email");
                Ok(item.event.blocks.clone())
            }
        }
    }

    fn preview_ttl(&self) -> Duration {
        Duration::from_secs(365 * 24 * 3600)
    }
}

/// A message's text for the ranker and agents: its plain-text part, or,
/// for HTML-only mail, the HTML as Markdown (mail-parser's own conversion
/// drops links, tables and lists).
fn body_text(m: &mail_parser::Message) -> String {
    let html_only = m.text_part(0).is_some_and(|p| p.is_text_html());
    if html_only
        && let Some(md) = m
            .body_html(0)
            .and_then(|h| crate::knowledge::extract::html_to_markdown(&h))
    {
        return md;
    }
    m.body_text(0).map(|b| b.into_owned()).unwrap_or_default()
}

#[cfg(test)]
mod tests {

    #[test]
    fn html_only_mail_reads_as_markdown() {
        let raw = b"From: a@b.c\r\nSubject: x\r\nContent-Type: text/html\r\n\r\n<p>Ship it: <a href=\"https://x.dev/pr/1\">PR 1</a></p><ul><li>one</li><li>two</li></ul>";
        let m = mail_parser::MessageParser::default()
            .parse(&raw[..])
            .unwrap();
        let body = super::body_text(&m);
        assert!(body.contains("- one\n- two"), "{body}");
        let plain = b"From: a@b.c\r\nSubject: x\r\n\r\nJust text";
        let m = mail_parser::MessageParser::default()
            .parse(&plain[..])
            .unwrap();
        assert_eq!(super::body_text(&m).trim(), "Just text");
    }

    use super::*;

    fn settings(bulk: BulkMode) -> Settings {
        Settings {
            host: "mail.example.com".into(),
            port: 993,
            username: "vince@example.com".into(),
            auth: Auth::Password("x".into()),
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
    fn signs_in_with_a_password_or_google_microsoft() {
        let mut cfg = ConnectorConfig::on();
        cfg.settings.insert("host".into(), "imap.gmail.com".into());
        cfg.settings
            .insert("username".into(), "me@gmail.com".into());
        let e = Settings::from_config(&cfg).unwrap_err().to_string();
        assert!(e.contains("password"), "{e}");
        cfg.settings.insert("auth".into(), "google".into());
        assert!(matches!(
            Settings::from_config(&cfg).unwrap().auth,
            Auth::OAuth(crate::mail_oauth::Provider::Google)
        ));
        cfg.settings.insert("auth".into(), "".into());
        cfg.settings.insert("password".into(), "pw".into());
        assert!(matches!(
            Settings::from_config(&cfg).unwrap().auth,
            Auth::Password(_)
        ));
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

    #[test]
    fn renders_email_and_links_to_webmail() {
        let raw =
            b"From: Ann <ann@acme.com>\r\nTo: v@x.dev\r\nSubject: Hi\r\nMIME-Version: 1.0\r\n\
Content-Type: multipart/related; boundary=b\r\n\r\n--b\r\nContent-Type: text/html\r\n\r\n\
<p>Hello <img src=\"cid:logo\"></p>\r\n--b\r\nContent-Type: image/png\r\nContent-ID: <logo>\r\n\
Content-Transfer-Encoding: base64\r\n\r\naGk=\r\n--b--\r\n";
        let blocks = render_message(raw);
        let html = blocks.iter().find_map(|b| match b {
            Block::Html { html } => Some(html.clone()),
            _ => None,
        });
        assert!(html.unwrap().contains("data:image/png;base64,aGk="));
        assert_eq!(base64(b"hello"), "aGVsbG8=");

        assert_eq!(
            open_url(None, "heracles.mxrouting.net", "INBOX", 7, "a@b").as_deref(),
            Some(
                "https://heracles.mxrouting.net/roundcube/?_task=mail&_mbox=INBOX&_uid=7&_action=show"
            )
        );
        assert!(
            open_url(None, "imap.gmail.com", "INBOX", 1, "x@y")
                .unwrap()
                .contains("rfc822msgid%3Ax%40y")
        );
        assert_eq!(
            open_url(Some("mail.me.dev"), "h", "INBOX", 1, "x").as_deref(),
            Some("https://mail.me.dev")
        );
        assert_eq!(open_url(None, "mail.example.org", "INBOX", 1, "x"), None);
    }

    #[test]
    fn finds_imap_hosts_from_mx_and_autoconfig() {
        let doh = json!({"Answer": [
            {"type": 15, "data": "20 backup.mxrouting.net."},
            {"type": 15, "data": "10 heracles.mxrouting.net."},
            {"type": 5, "data": "ignored"}
        ]});
        let mx = parse_mx(&doh);
        assert_eq!(mx, vec!["heracles.mxrouting.net", "backup.mxrouting.net"]);
        let p = known_provider(&mx, "vince.dev").unwrap();
        assert_eq!(
            (p.0, p.1.as_deref()),
            ("MXroute", Some("heracles.mxrouting.net"))
        );
        assert_eq!(known_provider(&[], "gmail.com").unwrap().0, "Google");
        assert!(known_provider(&["mx.example.org".into()], "example.org").is_none());

        let xml = r#"<clientConfig><emailProvider id="x">
          <incomingServer type="pop3"><hostname>pop.x.com</hostname><port>995</port><socketType>SSL</socketType></incomingServer>
          <incomingServer type="imap"><hostname>imap.%EMAILDOMAIN%</hostname><port>993</port><socketType>SSL</socketType></incomingServer>
          <incomingServer type="imap"><hostname>plain.x.com</hostname><port>143</port><socketType>STARTTLS</socketType></incomingServer>
        </emailProvider></clientConfig>"#;
        assert_eq!(
            parse_autoconfig(xml, "a@x.com", "x.com"),
            vec![("imap.x.com".to_string(), 993)]
        );
    }
}
