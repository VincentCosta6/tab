//! Agents using tab's browser. Read `docs/agent-browser.md` before changing
//! anything here: it holds the plan and the rules this module enforces.
//!
//! Phase 1 is read-only and logged out. An agent in a chat the person drives
//! opens a page; it shows in the browser dock as an agent tab (its own empty,
//! throwaway data store: no logins, no cookies shared with the person's
//! tabs), and the agent gets the page as text, fetched through the gateway
//! with no cookies at all. Nothing here clicks, types or submits.
//!
//! Everything that decides what an agent may reach lives here, in plain code
//! the page and the model can't influence: the URL rules ([`check`]), the
//! redaction of secrets ([`redact`]), and the per-chat limits ([`Limits`]).

use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use url::{Host, Url};

use crate::gateway::{Call, Gateway};
use crate::knowledge::extract;
use crate::sandbox::Network;

/// What agents may do with the browser (Settings → Browser). Off until the
/// person turns it on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentBrowsing {
    #[default]
    Off,
    /// Open and read public pages, logged out. Nothing is clicked or typed.
    ReadOnly,
}

/// Agent tabs open at once, per chat.
pub const MAX_TABS: usize = 4;
/// Page reads per chat per minute.
pub const READS_PER_MINUTE: usize = 30;
/// Redirects followed for one read; each hop is checked again.
const MAX_REDIRECTS: u8 = 5;
/// Text returned for one read, by default and at most.
pub const DEFAULT_CHARS: usize = 12_000;
pub const MAX_CHARS: usize = 40_000;
/// Links listed per read.
const MAX_LINKS: usize = 60;

// ---------------------------------------------------------------- URL rules

/// The checks that need no network: scheme, credentials in the URL, and
/// hosts that are this computer or its network. The dock's navigation
/// handler runs these on every load of an agent tab, redirects included.
pub fn check_static(u: &Url) -> anyhow::Result<()> {
    anyhow::ensure!(
        matches!(u.scheme(), "http" | "https"),
        "agents can only open http and https pages"
    );
    anyhow::ensure!(
        u.username().is_empty() && u.password().is_none(),
        "that link has a user name or password in it, which agents can't open"
    );
    let local = "agents can't open pages on this computer or its network";
    match u.host() {
        None => anyhow::bail!("that link has no host"),
        Some(Host::Ipv4(ip)) => anyhow::ensure!(is_public(IpAddr::V4(ip)), local),
        Some(Host::Ipv6(ip)) => anyhow::ensure!(is_public(IpAddr::V6(ip)), local),
        Some(Host::Domain(d)) => {
            let d = d.trim_end_matches('.').to_ascii_lowercase();
            anyhow::ensure!(
                !(d == "localhost"
                    || d.ends_with(".localhost")
                    || d.ends_with(".local")
                    || d.ends_with(".internal")
                    || d.ends_with(".home.arpa")
                    || !d.contains('.')),
                local
            );
        }
    }
    Ok(())
}

fn is_public(ip: IpAddr) -> bool {
    crate::knowledge::web::is_public(ip)
}

/// Whether the chat's sandbox lets it reach `u`'s host. The browser runs in
/// tab, outside the sandbox, so it must never reach further than the chat
/// itself could.
pub fn network_allows(net: &Network, u: &Url) -> bool {
    let Some(host) = u.host_str() else {
        return false;
    };
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    match net {
        Network::Any => true,
        Network::None => false,
        Network::Domains(list) => list.iter().any(|d| {
            let d = d.trim().to_ascii_lowercase();
            if d == "*" {
                return true;
            }
            match d.strip_prefix("*.") {
                Some(base) => host == base || host.ends_with(&format!(".{base}")),
                None => host == d,
            }
        }),
    }
}

/// Everything, for a URL an agent is about to read: the static checks, the
/// chat's network, and that the host resolves only to public addresses.
pub async fn check(u: &Url, net: &Network) -> anyhow::Result<()> {
    check_static(u)?;
    anyhow::ensure!(
        network_allows(net, u),
        "this chat's sandbox doesn't let it reach {}",
        u.host_str().unwrap_or_default()
    );
    crate::knowledge::web::check_public(u).await
}

/// The site a URL belongs to: its registrable domain (`dash.cloudflare.com`
/// → `cloudflare.com`; `a.github.io` stays `a.github.io`).
pub fn site(u: &Url) -> Option<String> {
    let host = u.host_str()?.trim_end_matches('.').to_ascii_lowercase();
    Some(psl::domain_str(&host).unwrap_or(&host).to_string())
}

/// Parses what an agent passed as a URL; a bare host gets `https://`.
pub fn parse(input: &str) -> anyhow::Result<Url> {
    let t = input.trim();
    anyhow::ensure!(!t.is_empty(), "give a URL");
    let u = if t.contains("://") {
        Url::parse(t)?
    } else {
        Url::parse(&format!("https://{t}"))?
    };
    Ok(u)
}

// ---------------------------------------------------------------- redaction

const HIDDEN: &str = "[secret hidden]";

/// Prefixes of credentials providers hand out on setup pages.
const KEY_PREFIXES: &[&str] = &[
    "sk_live_",
    "sk_test_",
    "rk_live_",
    "rk_test_",
    "whsec_",
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "ghr_",
    "github_pat_",
    "glpat-",
    "xoxb-",
    "xoxp-",
    "xoxa-",
    "xapp-",
    "sk-ant-",
    "sk-proj-",
    "sk-",
    "AKIA",
    "ASIA",
    "AIza",
    "ya29.",
    "npm_",
    "pypi-",
    "SG.",
    "shpat_",
    "dop_v1_",
    "hf_",
];

fn token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | '+' | '=')
}

/// Bits per character: random keys score high, words and hex ids lower.
fn entropy(s: &str) -> f64 {
    let mut counts: HashMap<char, usize> = HashMap::new();
    for c in s.chars() {
        *counts.entry(c).or_default() += 1;
    }
    let n = s.chars().count() as f64;
    counts
        .values()
        .map(|&c| {
            let p = c as f64 / n;
            -p * p.log2()
        })
        .sum()
}

/// Whether a run of token characters looks like a credential.
fn looks_secret(tok: &str) -> bool {
    let t = tok.trim_matches(|c: char| matches!(c, '.' | '/' | '=' | '-' | '+'));
    if t.len() < 16 {
        return false;
    }
    if KEY_PREFIXES.iter().any(|p| t.starts_with(p)) {
        return true;
    }
    // A JSON Web Token: three base64url parts, the first a JSON header.
    if t.starts_with("eyJ") && t.matches('.').count() == 2 {
        return true;
    }
    // Long, mixed letters and digits, and random-looking. Paths, URLs and
    // hex ids (commit SHAs, UUIDs) stay: they're under the entropy bar or
    // have structure.
    if t.len() >= 32 && !t.contains('/') && !t.contains('.') {
        let digits = t.chars().any(|c| c.is_ascii_digit());
        let upper = t.chars().any(|c| c.is_ascii_uppercase());
        let lower = t.chars().any(|c| c.is_ascii_lowercase());
        return digits && upper && lower && entropy(t) >= 4.2;
    }
    false
}

/// Hides credentials in page text before an agent sees it: private key
/// blocks, provider keys and tokens, and long random strings. Returns the
/// text and how many were hidden, so the agent can be told to ask the
/// person (the vault flow) instead.
pub fn redact(text: &str) -> (String, usize) {
    let mut hidden = 0;
    // Private key blocks, whole.
    let mut s = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find("-----BEGIN") {
        let (before, from) = rest.split_at(i);
        s.push_str(before);
        let head_end = from[10..].find("-----").map(|j| 10 + j + 5);
        let is_key = head_end.is_some_and(|e| from[..e].contains("PRIVATE KEY"));
        match (is_key, from.find("-----END")) {
            (true, Some(end)) => {
                let tail = from[end + 8..]
                    .find("-----")
                    .map(|j| end + 8 + j + 5)
                    .unwrap_or(from.len());
                s.push_str(HIDDEN);
                hidden += 1;
                rest = &from[tail..];
            }
            (true, None) => {
                s.push_str(HIDDEN);
                hidden += 1;
                rest = "";
            }
            _ => {
                s.push_str("-----BEGIN");
                rest = &from[10..];
            }
        }
    }
    s.push_str(rest);
    // Tokens.
    let mut out = String::with_capacity(s.len());
    let mut tok = String::new();
    let flush = |tok: &mut String, out: &mut String, hidden: &mut usize| {
        if looks_secret(tok) {
            out.push_str(HIDDEN);
            *hidden += 1;
        } else {
            out.push_str(tok);
        }
        tok.clear();
    };
    for c in s.chars() {
        if token_char(c) {
            tok.push(c);
        } else {
            flush(&mut tok, &mut out, &mut hidden);
            out.push(c);
        }
    }
    flush(&mut tok, &mut out, &mut hidden);
    (out, hidden)
}

// ---------------------------------------------------------------- tabs

/// An agent tab in the dock.
#[derive(Debug, Clone)]
pub struct AgentTab {
    pub session_id: String,
    pub url: Url,
    /// The links on the last page read, so the agent can follow one by number.
    pub links: Vec<Url>,
}

static TABS: LazyLock<Mutex<HashMap<String, AgentTab>>> = LazyLock::new(Default::default);

/// Registers a new agent tab for `session_id`, or fails at [`MAX_TABS`].
pub fn open_tab(session_id: &str, url: Url) -> anyhow::Result<String> {
    let mut tabs = TABS.lock().unwrap();
    let mine = tabs.values().filter(|t| t.session_id == session_id).count();
    anyhow::ensure!(
        mine < MAX_TABS,
        "this chat already has {MAX_TABS} browser tabs open; close one with tab_browser_close"
    );
    let id = format!("agent-{}", &uuid::Uuid::new_v4().simple().to_string()[..10]);
    tabs.insert(
        id.clone(),
        AgentTab {
            session_id: session_id.to_string(),
            url,
            links: Vec::new(),
        },
    );
    Ok(id)
}

/// The agent tab `id`, if it is one. The desktop shell asks this before
/// creating a webview, so an agent tab is always built logged out and
/// locked down, whatever the UI asked for.
pub fn agent_tab(id: &str) -> Option<AgentTab> {
    TABS.lock().unwrap().get(id).cloned()
}

/// The tab, if it belongs to `session_id`. A chat can't touch another's tabs.
pub fn tab_of(session_id: &str, id: &str) -> anyhow::Result<AgentTab> {
    agent_tab(id)
        .filter(|t| t.session_id == session_id)
        .ok_or_else(|| {
            anyhow::anyhow!("no browser tab {id} in this chat; open one with tab_browser_open")
        })
}

pub fn update_tab(id: &str, url: Url, links: Vec<Url>) {
    if let Some(t) = TABS.lock().unwrap().get_mut(id) {
        t.url = url;
        t.links = links;
    }
}

pub fn close_tab(id: &str) -> Option<AgentTab> {
    TABS.lock().unwrap().remove(id)
}

/// Closes every tab of a chat (it ended, or the person pressed Stop).
pub fn close_session(session_id: &str) -> Vec<String> {
    let mut tabs = TABS.lock().unwrap();
    let ids: Vec<String> = tabs
        .iter()
        .filter(|(_, t)| t.session_id == session_id)
        .map(|(id, _)| id.clone())
        .collect();
    for id in &ids {
        tabs.remove(id);
    }
    ids
}

// ---------------------------------------------------------------- limits

/// Reads per chat in the last minute, to stop runaway loops.
#[derive(Default)]
pub struct Limits {
    reads: Mutex<HashMap<String, VecDeque<Instant>>>,
}

impl Limits {
    pub fn take(&self, session_id: &str) -> anyhow::Result<()> {
        self.take_at(session_id, Instant::now())
    }

    fn take_at(&self, session_id: &str, now: Instant) -> anyhow::Result<()> {
        let mut reads = self.reads.lock().unwrap();
        let q = reads.entry(session_id.to_string()).or_default();
        while q
            .front()
            .is_some_and(|t| now.duration_since(*t) >= Duration::from_secs(60))
        {
            q.pop_front();
        }
        anyhow::ensure!(
            q.len() < READS_PER_MINUTE,
            "that's {READS_PER_MINUTE} page reads in a minute; slow down and read what you have"
        );
        q.push_back(now);
        Ok(())
    }
}

pub static LIMITS: LazyLock<Limits> = LazyLock::new(Limits::default);

// ---------------------------------------------------------------- reading

/// A page as an agent gets it.
#[derive(Debug, Clone, Serialize)]
pub struct Read {
    pub url: String,
    pub title: Option<String>,
    pub text: String,
    /// Credentials hidden from `text`.
    pub hidden: usize,
    /// Whether `text` was cut to fit.
    pub truncated: bool,
    #[serde(skip)]
    pub links: Vec<Url>,
}

/// Fetches `start` logged out (no cookies, through the gateway), following
/// redirects itself so each hop passes [`check`]. Returns the page as
/// redacted Markdown, focused on `query` when given.
pub async fn read(
    gw: &Gateway,
    start: Url,
    net: &Network,
    query: Option<&str>,
    max_chars: usize,
) -> anyhow::Result<Read> {
    let mut u = start;
    let mut hops = 0;
    let res = loop {
        check(&u, net).await?;
        let call =
            Call::get("web", "browser", "an agent reading a page", u.as_str()).no_redirects();
        let res = gw.send(call).await?;
        if (300..400).contains(&res.status) {
            let next = res
                .headers
                .get(reqwest::header::LOCATION)
                .and_then(|l| l.to_str().ok())
                .and_then(|l| u.join(l).ok())
                .ok_or_else(|| anyhow::anyhow!("{u} redirects nowhere"))?;
            hops += 1;
            anyhow::ensure!(hops <= MAX_REDIRECTS, "{u} redirects too many times");
            u = next;
            continue;
        }
        break res;
    };
    let kind = res
        .headers
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("text/html")
        .to_ascii_lowercase();
    let html = kind.contains("html");
    anyhow::ensure!(
        !(matches!(res.status, 401 | 403) || (html && asks_password(&res.body))),
        "{u} needs a sign-in. Agents only browse logged out for now: ask the person to open it in their own tab"
    );
    anyhow::ensure!(res.status < 400, "{u} answered {}", res.status);
    let page = if html {
        extract::html(&res.body)
    } else if kind.starts_with("text/") || kind.contains("json") || kind.contains("xml") {
        extract::Page {
            title: None,
            text: res.body.clone(),
            links: Vec::new(),
        }
    } else {
        anyhow::bail!("{u} isn't a page agents can read ({kind})");
    };
    let mut links: Vec<Url> = Vec::new();
    for href in &page.links {
        if let Ok(mut l) = u.join(href.trim()) {
            l.set_fragment(None);
            if matches!(l.scheme(), "http" | "https") && !links.contains(&l) {
                links.push(l);
            }
        }
        if links.len() >= MAX_LINKS {
            break;
        }
    }
    let (text, hidden) = redact(&page.text);
    let (text, truncated) = focus(&text, query, max_chars.clamp(1_000, MAX_CHARS));
    let title = page.title.map(|t| redact(&t).0);
    Ok(Read {
        url: u.to_string(),
        title,
        text,
        hidden,
        truncated,
        links,
    })
}

fn asks_password(html: &str) -> bool {
    let html = html.to_ascii_lowercase();
    ["type=\"password\"", "type='password'", "type=password"]
        .iter()
        .any(|p| html.contains(p))
}

/// Cuts Markdown to `max` characters. With a query, keeps the sections
/// (split at headings) that mention its words most, in page order; with
/// none, or no section matching, the start of the page.
pub fn focus(md: &str, query: Option<&str>, max: usize) -> (String, bool) {
    if md.chars().count() <= max {
        return (md.to_string(), false);
    }
    let words: Vec<String> = query
        .unwrap_or_default()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 3)
        .map(str::to_lowercase)
        .collect();
    let mut sections: Vec<String> = Vec::new();
    for line in md.lines() {
        if line.starts_with('#') || sections.is_empty() {
            sections.push(String::new());
        }
        let s = sections.last_mut().unwrap();
        s.push_str(line);
        s.push('\n');
    }
    let scores: Vec<usize> = sections
        .iter()
        .map(|s| {
            let l = s.to_lowercase();
            words.iter().map(|w| l.matches(w.as_str()).count()).sum()
        })
        .collect();
    if words.is_empty() || scores.iter().all(|&s| s == 0) {
        return (md.chars().take(max).collect(), true);
    }
    let mut order: Vec<usize> = (0..sections.len()).filter(|&i| scores[i] > 0).collect();
    order.sort_by(|a, b| scores[*b].cmp(&scores[*a]).then(a.cmp(b)));
    let mut keep = Vec::new();
    let mut used = 0;
    for i in order {
        let n = sections[i].chars().count();
        if used + n > max {
            continue;
        }
        used += n;
        keep.push(i);
    }
    if keep.is_empty() {
        let best = (0..sections.len()).max_by_key(|&i| scores[i]).unwrap();
        return (sections[best].chars().take(max).collect(), true);
    }
    keep.sort();
    let out: Vec<&str> = keep.iter().map(|&i| sections[i].as_str()).collect();
    (out.join("\n…\n\n"), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn static_checks_refuse_schemes_credentials_and_local_hosts() {
        for bad in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "data:text/html,hi",
            "about:blank",
            "tab://item/1",
            "ftp://example.com/",
            "mailto:a@b.com",
            "https://user:pw@example.com/",
            "https://dash.cloudflare.com@evil.com/",
            "http://localhost:8080/",
            "http://LOCALHOST./",
            "http://app.localhost/",
            "http://printer.local/",
            "http://db.internal/",
            "http://router.home.arpa/",
            "http://intranet/",
            "http://127.0.0.1/",
            "http://127.1/",
            "http://2130706433/",
            "http://0x7f000001/",
            "http://0177.0.0.1/",
            "http://0.0.0.0/",
            "http://10.0.0.1/",
            "http://172.16.5.4/",
            "http://192.168.1.1/",
            "http://169.254.169.254/latest/meta-data/",
            "http://100.64.0.1/",
            "http://[::1]/",
            "http://[::ffff:127.0.0.1]/",
            "http://[fe80::1]/",
            "http://[fd00::1]/",
        ] {
            let parsed = Url::parse(bad);
            assert!(
                parsed.map_or(true, |p| check_static(&p).is_err()),
                "{bad} should be refused"
            );
        }
        for good in [
            "https://developers.cloudflare.com/dns/",
            "http://example.com/",
            "https://8.8.8.8/",
            "https://github.com/settings",
        ] {
            assert!(check_static(&u(good)).is_ok(), "{good} should pass");
        }
    }

    #[test]
    fn the_sandbox_network_bounds_the_browser() {
        let page = u("https://docs.stripe.com/webhooks");
        assert!(network_allows(&Network::Any, &page));
        assert!(!network_allows(&Network::None, &page));
        let only = |d: &[&str]| Network::Domains(d.iter().map(|s| s.to_string()).collect());
        assert!(network_allows(&only(&["*.stripe.com"]), &page));
        assert!(network_allows(&only(&["docs.stripe.com"]), &page));
        assert!(!network_allows(&only(&["stripe.com"]), &page));
        assert!(!network_allows(&only(&["*.github.com"]), &page));
        assert!(!network_allows(
            &only(&["*.stripe.com"]),
            &u("https://stripe.com.evil.net/")
        ));
        assert!(network_allows(&only(&["*"]), &page));
    }

    #[tokio::test]
    async fn check_refuses_what_the_chat_cant_reach_before_any_lookup() {
        let err = check(&u("https://example.com/"), &Network::None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("sandbox"));
        assert!(
            check(&u("http://127.0.0.1:1/"), &Network::Any)
                .await
                .is_err()
        );
    }

    #[test]
    fn sites_are_registrable_domains() {
        assert_eq!(
            site(&u("https://dash.cloudflare.com/x")).as_deref(),
            Some("cloudflare.com")
        );
        assert_eq!(
            site(&u("https://alice.github.io/")).as_deref(),
            Some("alice.github.io")
        );
        assert_ne!(
            site(&u("https://alice.github.io/")),
            site(&u("https://bob.github.io/"))
        );
        assert_eq!(
            site(&u("https://WWW.Example.co.uk./")).as_deref(),
            Some("example.co.uk")
        );
        // Punycode look-alikes are their own site.
        assert_ne!(
            site(&u("https://аррӏе.com/")),
            site(&u("https://apple.com/"))
        );
    }

    #[test]
    fn parse_adds_https_to_bare_hosts() {
        assert_eq!(
            parse("docs.github.com/en").unwrap().as_str(),
            "https://docs.github.com/en"
        );
        assert!(parse("  ").is_err());
    }

    #[test]
    fn redact_hides_provider_keys_and_random_tokens() {
        // Built at run time, so secret scanners don't take the fixtures for
        // real keys.
        let key = |prefix: &str, body: &str| format!("{prefix}{body}");
        let page = format!(
            "Your secret key: {}\nGitHub token {}\nAWS {} and webhook {}\n\
             jwt eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0In0.dGhpc2lzYXNpZ25hdHVyZQ\n\
             random Zx8Kq2Lm9Pw4Rt7Yb3Nc6Vd1Hf5Gj0Ts\nanthropic {}",
            key(&["sk", "live", "51"].join("_"), "HxAbCdEfGhIjKlMnOpQrStUv"),
            key(
                &["gh", "p_"].concat(),
                "aBcDeFgHiJkLmNoPqRsTuVwXyZ0123456789"
            ),
            key(&["AK", "IA"].concat(), "IOSFODNN7EXAMPLE"),
            key(&["wh", "sec_"].concat(), "abcdefghijklmnop1234"),
            key(&["sk", "-ant-api03-"].concat(), "AbCdEfGhIjKlMnOpQrSt"),
        );
        let (out, n) = redact(&page);
        assert_eq!(n, 7, "{out}");
        for leak in [
            "sk_live_51",
            "ghp_aBc",
            "AKIAIOSF",
            "whsec_",
            "eyJhbGci",
            "Zx8Kq2",
            "sk-ant-",
        ] {
            assert!(!out.contains(leak), "{leak} leaked: {out}");
        }
        assert!(out.contains("Your secret key: [secret hidden]"));
    }

    #[test]
    fn redact_hides_private_key_blocks() {
        let page = "before\n-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA\nAAAA\n-----END OPENSSH PRIVATE KEY-----\nafter";
        let (out, n) = redact(page);
        assert_eq!(n, 1);
        assert_eq!(out, "before\n[secret hidden]\nafter");
        // A public certificate isn't a secret.
        let cert = "-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----";
        assert_eq!(redact(cert).0, cert);
    }

    #[test]
    fn redact_keeps_ordinary_text_ids_and_urls() {
        let page = "Set the CNAME www to app.example.com. Commit 9fceb02d0ae598e95dc970b74767f19372d61af8, \
                    request 123e4567-e89b-12d3-a456-426614174000, see https://developers.cloudflare.com/dns/manage-dns-records/how-to/create-dns-records/ \
                    and internationalization-configuration-settings.";
        let (out, n) = redact(page);
        assert_eq!(n, 0, "{out}");
        assert_eq!(out, page);
    }

    #[test]
    fn focus_keeps_matching_sections_in_page_order() {
        let md = format!(
            "# Intro\n{}\n## Webhooks\nAdd an endpoint for webhooks.\n## Billing\n{}\n## Signing webhooks\nVerify the webhook signature.\n",
            "x ".repeat(400),
            "y ".repeat(400)
        );
        let (out, cut) = focus(&md, Some("webhook signature"), 200);
        assert!(cut);
        assert!(out.contains("Verify the webhook signature"));
        assert!(out.find("## Webhooks").unwrap() < out.find("## Signing").unwrap());
        assert!(!out.contains("Billing"));
        let (start, cut) = focus(&md, None, 50);
        assert!(cut && start.starts_with("# Intro"));
        assert_eq!(focus("short", Some("x"), 100), ("short".into(), false));
    }

    #[test]
    fn tabs_belong_to_one_chat_and_are_capped() {
        let s = format!("chat-{}", uuid::Uuid::new_v4());
        let other = format!("chat-{}", uuid::Uuid::new_v4());
        let ids: Vec<String> = (0..MAX_TABS)
            .map(|_| open_tab(&s, u("https://example.com/")).unwrap())
            .collect();
        assert!(open_tab(&s, u("https://example.com/")).is_err());
        assert!(tab_of(&s, &ids[0]).is_ok());
        assert!(tab_of(&other, &ids[0]).is_err());
        assert!(agent_tab(&ids[0]).is_some());
        assert!(agent_tab("browser-tab-the-person-opened").is_none());
        close_tab(&ids[0]);
        assert!(open_tab(&s, u("https://example.com/")).is_ok());
        assert_eq!(close_session(&s).len(), MAX_TABS);
        assert!(agent_tab(&ids[1]).is_none());
    }

    #[test]
    fn reads_are_rate_limited_per_chat() {
        let l = Limits::default();
        let t0 = Instant::now();
        for _ in 0..READS_PER_MINUTE {
            l.take_at("a", t0).unwrap();
        }
        assert!(l.take_at("a", t0).is_err());
        assert!(l.take_at("b", t0).is_ok());
        assert!(l.take_at("a", t0 + Duration::from_secs(61)).is_ok());
    }
}
