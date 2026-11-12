//! Reads a website for a project: the page, and the pages it links to on
//! the same site, a level or two deep. Every request goes through the
//! gateway (audited, rate limited), respects robots.txt, and never reaches
//! this computer or its network: an agent can add a website, so a link to
//! `localhost` or `192.168.…` mustn't put what's there into search.

use std::collections::{HashSet, VecDeque};
use std::net::IpAddr;
use std::time::Duration;

use url::Url;

use super::chunk::Doc;
use super::extract;
use crate::gateway::{Call, Gateway};

/// Most pages read from one site, by depth.
pub fn max_pages(depth: u32) -> usize {
    match depth {
        0 => 1,
        1 => 30,
        _ => 80,
    }
}

/// Links not worth fetching as pages.
const NOT_PAGES: &[&str] = &[
    ".png", ".jpg", ".jpeg", ".gif", ".webp", ".svg", ".ico", ".css", ".js", ".zip", ".gz", ".mp4",
    ".mov", ".mp3", ".woff", ".woff2", ".ttf", ".pdf", ".dmg", ".exe", ".xml", ".json",
];

fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => {
            !(v.is_private()
                || v.is_loopback()
                || v.is_link_local()
                || v.is_unspecified()
                || v.is_broadcast()
                || v.is_documentation()
                // 100.64.0.0/10, carrier-grade NAT (and Tailscale).
                || (v.octets()[0] == 100 && (v.octets()[1] & 0xc0) == 64))
        }
        IpAddr::V6(v) => {
            if let Some(v4) = v.to_ipv4_mapped() {
                return is_public(IpAddr::V4(v4));
            }
            let first = v.segments()[0];
            !(v.is_loopback()
                || v.is_unspecified()
                || (first & 0xfe00) == 0xfc00
                || (first & 0xffc0) == 0xfe80)
        }
    }
}

/// Fails unless `u` is http(s) on a host that resolves only to public addresses.
pub async fn check_public(u: &Url) -> anyhow::Result<()> {
    anyhow::ensure!(
        matches!(u.scheme(), "http" | "https"),
        "only http and https links can be read"
    );
    let host = u
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("that link has no host"))?;
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    anyhow::ensure!(
        !(bare.eq_ignore_ascii_case("localhost")
            || bare.ends_with(".localhost")
            || bare.ends_with(".local")),
        "tab doesn't read sites on this computer or its network"
    );
    if let Ok(ip) = bare.parse::<IpAddr>() {
        anyhow::ensure!(
            is_public(ip),
            "tab doesn't read sites on this computer or its network"
        );
        return Ok(());
    }
    let port = u.port_or_known_default().unwrap_or(443);
    let addrs: Vec<_> = tokio::net::lookup_host((bare, port)).await?.collect();
    anyhow::ensure!(!addrs.is_empty(), "{host} doesn't resolve");
    anyhow::ensure!(
        addrs.iter().all(|a| is_public(a.ip())),
        "{host} points at this computer or its network, which tab doesn't read"
    );
    Ok(())
}

/// What robots.txt asks every crawler to stay out of.
fn disallowed(robots: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut applies = false;
    let mut in_agents = false;
    for line in robots.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let (k, v) = (k.trim().to_ascii_lowercase(), v.trim());
        match k.as_str() {
            "user-agent" => {
                if !in_agents {
                    applies = false;
                }
                in_agents = true;
                applies |= v == "*" || v.eq_ignore_ascii_case("tab");
            }
            "disallow" => {
                in_agents = false;
                if applies && !v.is_empty() {
                    out.push(v.to_string());
                }
            }
            _ => in_agents = false,
        }
    }
    out
}

fn same_site(a: &Url, b: &Url) -> bool {
    let h = |u: &Url| {
        u.host_str()
            .map(|h| h.trim_start_matches("www.").to_ascii_lowercase())
    };
    h(a) == h(b)
}

/// A link on `base`, as a page to read: absolute, same site, no fragment.
fn page_link(base: &Url, start: &Url, href: &str) -> Option<Url> {
    let mut u = base.join(href.trim()).ok()?;
    u.set_fragment(None);
    let path = u.path().to_ascii_lowercase();
    (matches!(u.scheme(), "http" | "https")
        && same_site(&u, start)
        && !NOT_PAGES.iter().any(|e| path.ends_with(e)))
    .then_some(u)
}

async fn get(gw: &Gateway, u: &Url) -> anyhow::Result<crate::gateway::Response> {
    check_public(u).await?;
    let call = Call::get("web", "knowledge", "read a project's website", u.as_str()).no_redirects();
    Ok(gw.send(call).await?)
}

/// Reads the site from `start`, following links on it `depth` levels deep.
/// Returns its pages as documents (the start page first).
pub async fn crawl(gw: &Gateway, start: &str, depth: u32) -> anyhow::Result<Vec<Doc>> {
    let start = Url::parse(start).or_else(|_| Url::parse(&format!("https://{start}")))?;
    check_public(&start).await?;
    let robots = match start.join("/robots.txt") {
        Ok(r) => match get(gw, &r).await {
            Ok(res) if res.status == 200 => disallowed(&res.body),
            _ => Vec::new(),
        },
        Err(_) => Vec::new(),
    };
    let limit = max_pages(depth.min(2));
    let mut seen: HashSet<String> = HashSet::new();
    let mut queue = VecDeque::from([(start.clone(), 0u32, 0u8)]);
    seen.insert(start.to_string());
    let mut docs = Vec::new();
    let mut first_error = None;
    let mut fetched = 0;
    while let Some((u, d, hops)) = queue.pop_front() {
        if docs.len() >= limit || fetched >= limit * 2 {
            break;
        }
        if robots.iter().any(|p| u.path().starts_with(p.as_str())) && u != start {
            continue;
        }
        if fetched > 0 {
            // Be gentle with small sites.
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        fetched += 1;
        let res = match get(gw, &u).await {
            Ok(r) => r,
            Err(e) => {
                tracing::debug!(url = %u, error = %e, "page not read");
                first_error.get_or_insert(e);
                continue;
            }
        };
        if (300..400).contains(&res.status) {
            let next = res
                .headers
                .get(reqwest::header::LOCATION)
                .and_then(|l| l.to_str().ok())
                .and_then(|l| u.join(l).ok());
            // A start page that moves (http → https, a trailing slash) is
            // still the start; other redirects stay on the site.
            if let Some(n) = next.filter(|n| hops < 5 && (same_site(n, &start) || u == start))
                && seen.insert(n.to_string())
            {
                queue.push_front((n, d, hops + 1));
            }
            continue;
        }
        let kind = res
            .headers
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("text/html")
            .to_ascii_lowercase();
        let page = if kind.contains("html") {
            extract::html(&res.body)
        } else if kind.starts_with("text/") {
            extract::Page {
                title: None,
                text: res.body.clone(),
                links: Vec::new(),
            }
        } else {
            continue;
        };
        if d < depth {
            for href in &page.links {
                if let Some(l) = page_link(&u, &start, href)
                    && seen.insert(l.to_string())
                {
                    queue.push_back((l, d + 1, 0));
                }
            }
        }
        if !page.text.trim().is_empty() {
            docs.push(Doc {
                title: page.title.unwrap_or_else(|| u.to_string()),
                source: Some(u.to_string()),
                text: page.text,
            });
        }
    }
    match (docs.is_empty(), first_error) {
        (true, Some(e)) => Err(e),
        (true, None) => anyhow::bail!("found no text on {start}"),
        _ => Ok(docs),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn refuses_this_computer_and_its_network() {
        for u in [
            "http://localhost:3000/",
            "http://127.0.0.1/",
            "http://192.168.1.10/admin",
            "http://10.0.0.1",
            "http://[::1]:8080/",
            "http://169.254.169.254/latest/meta-data",
            "http://100.100.1.1/",
            "http://printer.local/",
            "file:///etc/passwd",
        ] {
            assert!(check_public(&Url::parse(u).unwrap()).await.is_err(), "{u}");
        }
        assert!(
            check_public(&Url::parse("http://93.184.215.14/").unwrap())
                .await
                .is_ok()
        );
    }

    #[test]
    fn follows_robots_and_stays_on_the_site() {
        let r = disallowed(
            "User-agent: Googlebot\nDisallow: /g\n\nUser-agent: *\nDisallow: /admin\nDisallow:\nAllow: /x\n",
        );
        assert_eq!(r, vec!["/admin"]);
        let start = Url::parse("https://www.acme.dev/").unwrap();
        let base = Url::parse("https://acme.dev/docs/").unwrap();
        assert_eq!(
            page_link(&base, &start, "intro#top").unwrap().as_str(),
            "https://acme.dev/docs/intro"
        );
        assert!(page_link(&base, &start, "https://other.dev/").is_none());
        assert!(page_link(&base, &start, "/logo.png").is_none());
        assert!(page_link(&base, &start, "mailto:hi@acme.dev").is_none());
    }
}
