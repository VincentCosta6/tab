//! Proposing where items go before any arrive, from what a project's
//! checkouts already say (see `crate::store::routing` for the routing).
//!
//! A repo names the services it uses: a Sentry DSN or `SENTRY_PROJECT` in
//! its env or config, Linear keys (`TAB-123`) in its branches and commits,
//! the app names in `wrangler.toml`, `vercel.json`, `fly.toml` and
//! `package.json`. tab scans each project's checkouts for these
//! ([`scan_checkout`]), keeps what it found ([`RepoHint`]), and proposes
//! anchors for the containers they match, now and as new ones show up
//! ([`Store::propose_anchors`]). Containers named like a project or its repo
//! are proposed too. The person confirms each on the project's page.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;

use crate::store::{RepoHint, Store};

/// How deep into a checkout to look.
const MAX_DEPTH: usize = 3;
/// Files bigger than this aren't config.
const MAX_FILE: u64 = 256 * 1024;
/// Folders that are never config.
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    ".git",
    "target",
    "dist",
    "build",
    "out",
    "vendor",
    ".venv",
    "venv",
    ".next",
    ".turbo",
    ".cache",
    "coverage",
];
/// A key seen fewer times than this in branches and commits is noise.
const MIN_KEY_COUNT: usize = 3;
/// Things shaped like `ABC-123` that aren't tickets.
const NOT_KEYS: &[&str] = &[
    "UTF", "SHA", "ISO", "RFC", "HTTP", "TLS", "SSL", "CVE", "GPL", "AGPL", "LGPL", "MIT", "BSD",
    "APACHE", "WIP", "PR", "GH", "ES", "X", "V", "ID", "UUID", "MD", "AES", "RSA", "P",
];

/// What a checkout says about the services it uses.
pub fn scan_checkout(root: &Path) -> Vec<RepoHint> {
    let mut out = BTreeSet::new();
    let mut files = vec![];
    walk(root, root, 0, &mut files);
    for (path, rel) in files {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        scan_file(&rel, &text, &mut out);
    }
    for h in linear_keys(root) {
        out.insert(h);
    }
    out.into_iter().collect()
}

fn walk(root: &Path, dir: &Path, depth: usize, out: &mut Vec<(PathBuf, String)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let path = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            if depth < MAX_DEPTH && !SKIP_DIRS.contains(&name.as_str()) {
                walk(root, &path, depth + 1, out);
            }
            continue;
        }
        if !ft.is_file() || !interesting(&name, dir) {
            continue;
        }
        if e.metadata().is_ok_and(|m| m.len() <= MAX_FILE) {
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            out.push((path, rel));
        }
    }
}

/// Files that name services: env files, Sentry config, CI workflows and
/// deploy config.
fn interesting(name: &str, dir: &Path) -> bool {
    let lower = name.to_lowercase();
    lower == ".env"
        || lower.starts_with(".env.")
        || lower.ends_with(".env")
        || lower.contains("sentry")
        || matches!(
            lower.as_str(),
            "wrangler.toml"
                | "wrangler.json"
                | "wrangler.jsonc"
                | "vercel.json"
                | "fly.toml"
                | "package.json"
        )
        || (dir.ends_with(".github/workflows")
            && (lower.ends_with(".yml") || lower.ends_with(".yaml")))
}

/// `KEY = "value"`, `KEY: value` or `KEY=value` on one line, unquoted.
fn assigned<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let l = line
        .trim()
        .trim_start_matches("export ")
        .trim_start_matches("- ");
    let rest = l.strip_prefix(key)?;
    let rest = rest.trim_start().strip_prefix(['=', ':'])?.trim();
    let v = rest
        .split_once(" #")
        .map_or(rest, |(v, _)| v)
        .trim()
        .trim_matches(['"', '\'', ','])
        .trim();
    (!v.is_empty() && !v.contains("${{") && !v.starts_with('$')).then_some(v)
}

fn json_name(text: &str) -> Option<String> {
    // jsonc: drop whole-line comments.
    let text: String = text
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let name = v.get("name")?.as_str()?;
    // `@acme/web` → `web`.
    Some(name.rsplit('/').next().unwrap_or(name).to_string())
}

fn scan_file(rel: &str, text: &str, out: &mut BTreeSet<RepoHint>) {
    let file = rel.rsplit('/').next().unwrap_or(rel).to_lowercase();
    let mut hint = |service: &str, value: &str| {
        let value = value.trim();
        if !value.is_empty() && value.len() <= 100 {
            out.insert(RepoHint {
                service: service.into(),
                value: value.into(),
                file: rel.into(),
            });
        }
    };
    match file.as_str() {
        "package.json" | "vercel.json" | "wrangler.json" | "wrangler.jsonc" => {
            if let Some(n) = json_name(text) {
                let service = match file.as_str() {
                    "package.json" => "npm",
                    "vercel.json" => "vercel",
                    _ => "cloudflare",
                };
                hint(service, &n);
            }
        }
        "wrangler.toml" => {
            if let Some(n) = text.lines().find_map(|l| assigned(l, "name")) {
                hint("cloudflare", n);
            }
        }
        "fly.toml" => {
            if let Some(n) = text.lines().find_map(|l| assigned(l, "app")) {
                hint("fly", n);
            }
        }
        _ => {}
    }
    // Sentry: the org and project, from env, CI, sentry.properties
    // (`defaults.project`) or .sentryclirc (`project` under [defaults]).
    let mut org = None;
    let mut project = None;
    for line in text.lines() {
        for k in ["SENTRY_ORG", "defaults.org"] {
            org = org.or(assigned(line, k));
        }
        for k in ["SENTRY_PROJECT", "defaults.project"] {
            project = project.or(assigned(line, k));
        }
        if file == ".sentryclirc" {
            org = org.or(assigned(line, "org"));
            project = project.or(assigned(line, "project"));
        }
    }
    if let Some(p) = project {
        match org {
            Some(o) => hint("sentry", &format!("{o}/{p}")),
            None => hint("sentry", p),
        }
    }
    for id in dsn_projects(text) {
        hint("sentry_dsn", &id);
    }
}

/// The project ids of the Sentry DSNs in some text:
/// `https://<key>@o123.ingest.sentry.io/456` → `456`.
fn dsn_projects(text: &str) -> Vec<String> {
    let mut out = vec![];
    let mut rest = text;
    while let Some(i) = rest.find(".ingest.") {
        let after = &rest[i..];
        let host_end = after.find('/').unwrap_or(after.len());
        let host = &after[..host_end];
        if host.contains("sentry.io") || host.contains("sentry") {
            let id: String = after[host_end..]
                .trim_start_matches('/')
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            if !id.is_empty() {
                out.push(id);
            }
        }
        rest = &rest[i + ".ingest.".len()..];
    }
    out
}

/// Linear (or Jira) team keys that come up again and again in the
/// checkout's branch names and recent commits.
fn linear_keys(root: &Path) -> Vec<RepoHint> {
    let git = |args: &[&str]| -> String {
        std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default()
    };
    let branches = git(&[
        "for-each-ref",
        "--format=%(refname:short)",
        "refs/heads",
        "refs/remotes",
    ]);
    let commits = git(&["log", "-300", "--format=%s"]);
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for key in ticket_keys(&branches)
        .into_iter()
        .chain(ticket_keys(&commits))
    {
        *counts.entry(key).or_default() += 1;
    }
    counts
        .into_iter()
        .filter(|(_, n)| *n >= MIN_KEY_COUNT)
        .map(|(key, _)| RepoHint {
            service: "linear".into(),
            value: key,
            file: "branches and commits".into(),
        })
        .collect()
}

/// Every `ABC-123` in some text (any case: branch names are often
/// lowercase), as the uppercased team key.
fn ticket_keys(text: &str) -> Vec<String> {
    let mut out = vec![];
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() {
        // The start of a word.
        if b[i].is_ascii_alphabetic() && (i == 0 || !b[i - 1].is_ascii_alphanumeric()) {
            let start = i;
            while i < b.len() && b[i].is_ascii_alphanumeric() && i - start < 12 {
                i += 1;
            }
            let word = &text[start..i];
            if i + 1 < b.len()
                && b[i] == b'-'
                && b[i + 1].is_ascii_digit()
                && (2..=10).contains(&word.len())
                && word.bytes().all(|c| c.is_ascii_alphabetic())
            {
                let mut j = i + 1;
                while j < b.len() && b[j].is_ascii_digit() {
                    j += 1;
                }
                let key = word.to_uppercase();
                let ends = j == b.len() || !b[j].is_ascii_alphanumeric() || b[j] == b'-';
                if ends && !NOT_KEYS.contains(&key.as_str()) {
                    out.push(key);
                }
                i = j;
            }
            continue;
        }
        i += 1;
    }
    out
}

fn expand_home(p: &str) -> PathBuf {
    match p.strip_prefix("~/") {
        Some(rest) => std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .map(|h| Path::new(&h).join(rest))
            .unwrap_or_else(|_| PathBuf::from(p)),
        None => PathBuf::from(p),
    }
}

/// Scans every project's checkouts on this device and keeps what they say.
pub fn scan_projects(store: &Store) -> anyhow::Result<()> {
    for p in store.projects()? {
        let mut hints = BTreeSet::new();
        for r in store.project_repos(&p.id)? {
            let Some(path) = r.path.as_deref().filter(|p| !p.trim().is_empty()) else {
                continue;
            };
            let root = expand_home(path.trim());
            if root.is_dir() {
                hints.extend(scan_checkout(&root));
            }
        }
        store.repo_hints_replace(&p.id, &hints.into_iter().collect::<Vec<_>>())?;
    }
    Ok(())
}

/// Keeps proposals current: rescans checkouts every half hour (and when a
/// repo changes), and looks for new containers to propose for whenever
/// items come in.
pub fn spawn(store: Arc<Store>) {
    tokio::spawn(async move {
        const RESCAN: Duration = Duration::from_secs(30 * 60);
        let wake = store.routing_wake();
        // Let launch settle first.
        tokio::time::sleep(Duration::from_secs(20)).await;
        let mut scanned: Option<Instant> = None;
        let mut repos_seen = String::new();
        loop {
            // A repo added or moved is scanned right away.
            let repos = store
                .projects()
                .unwrap_or_default()
                .iter()
                .flat_map(|p| store.project_repos(&p.id).unwrap_or_default())
                .map(|r| format!("{}={}", r.repo, r.path.unwrap_or_default()))
                .collect::<Vec<_>>()
                .join(";");
            if scanned.is_none_or(|t| t.elapsed() >= RESCAN) || repos != repos_seen {
                let s = store.clone();
                match tokio::task::spawn_blocking(move || scan_projects(&s)).await {
                    Ok(Err(e)) => tracing::warn!(error = %e, "couldn't scan checkouts for routing"),
                    Err(e) => tracing::warn!(error = %e, "checkout scan stopped"),
                    Ok(Ok(())) => {}
                }
                scanned = Some(Instant::now());
                repos_seen = repos;
            }
            match store.propose_anchors(Utc::now()) {
                Ok(new) if !new.is_empty() => {
                    tracing::info!(count = new.len(), "proposed routing anchors")
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "couldn't propose routing anchors"),
            }
            tokio::select! {
                _ = wake.notified() => {
                    // Let a sync's batch land before looking.
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
                _ = tokio::time::sleep(Duration::from_secs(10 * 60)) => {}
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hints(files: &[(&str, &str)]) -> Vec<RepoHint> {
        let mut out = BTreeSet::new();
        for (rel, text) in files {
            scan_file(rel, text, &mut out);
        }
        out.into_iter().collect()
    }

    fn pairs(h: &[RepoHint]) -> Vec<(&str, &str)> {
        h.iter()
            .map(|h| (h.service.as_str(), h.value.as_str()))
            .collect()
    }

    #[test]
    fn finds_sentry_projects_and_dsns() {
        let h = hints(&[
            (
                ".env.production",
                "SENTRY_ORG=acme\nexport SENTRY_PROJECT=\"tab-desktop\" # the app\n",
            ),
            (
                "sentry.client.config.ts",
                "Sentry.init({ dsn: \"https://abc123@o4507.ingest.us.sentry.io/4508123\" })",
            ),
            (
                ".github/workflows/release.yml",
                "env:\n  SENTRY_PROJECT: ${{ secrets.P }}\n",
            ),
        ]);
        assert_eq!(
            pairs(&h),
            [("sentry", "acme/tab-desktop"), ("sentry_dsn", "4508123")]
        );
        let h = hints(&[(
            "ios/sentry.properties",
            "defaults.org=acme\ndefaults.project=tab-ios\n",
        )]);
        assert_eq!(pairs(&h), [("sentry", "acme/tab-ios")]);
        let h = hints(&[(".sentryclirc", "[defaults]\norg = acme\nproject = api\n")]);
        assert_eq!(pairs(&h), [("sentry", "acme/api")]);
    }

    #[test]
    fn finds_app_names() {
        let h = hints(&[
            (
                "package.json",
                r#"{ "name": "@acme/tab", "version": "1.0.0" }"#,
            ),
            (
                "site/wrangler.toml",
                "name = \"tab-site\"\nmain = \"src/index.ts\"\n",
            ),
            ("fly.toml", "app = 'tab-api'\n"),
            ("vercel.json", "// deploys\n{ \"name\": \"tab-web\" }"),
        ]);
        assert_eq!(
            pairs(&h),
            [
                ("cloudflare", "tab-site"),
                ("fly", "tab-api"),
                ("npm", "tab"),
                ("vercel", "tab-web")
            ]
        );
    }

    #[test]
    fn ticket_keys_skip_lookalikes() {
        let keys = ticket_keys(
            "vincent/tab-123-fix-routing\nFix TAB-9: utf-8 and SHA-256\nENG-42 follow-up (#164)\nv1-2",
        );
        assert_eq!(keys, ["TAB", "TAB", "ENG"]);
    }

    #[test]
    fn scans_a_checkout_on_disk() {
        let dir = std::env::temp_dir().join(format!("tab-routing-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("node_modules/x")).unwrap();
        std::fs::create_dir_all(dir.join("apps/web")).unwrap();
        std::fs::write(
            dir.join("node_modules/x/package.json"),
            r#"{"name":"left-pad"}"#,
        )
        .unwrap();
        std::fs::write(dir.join("apps/web/package.json"), r#"{"name":"web"}"#).unwrap();
        std::fs::write(dir.join(".env"), "SENTRY_PROJECT=web\n").unwrap();
        let h = scan_checkout(&dir);
        assert_eq!(pairs(&h), [("npm", "web"), ("sentry", "web")]);
        assert_eq!(h[0].file, "apps/web/package.json");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
