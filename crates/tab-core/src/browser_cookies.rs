//! Logins from the person's other browsers, for tab's in-app browser: reads
//! a browser's cookies so a site signed in there is signed in here too.
//!
//! - Chrome and its family (Brave, Edge, Arc, Vivaldi, Chromium) on macOS.
//!   Their cookies are sealed with a key kept in the login keychain, so
//!   macOS asks the person before tab can read it. On Windows Chrome seals
//!   them to itself (app-bound encryption), so tab can't.
//! - Firefox everywhere: its cookies are plain SQLite.
//!
//! Nothing here keeps a cookie: the caller hands them to the webview.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use serde::Serialize;

/// A browser profile tab can read cookies from.
#[derive(Debug, Clone)]
pub struct Source {
    /// Stable for the profile: its cookie file's path.
    pub id: String,
    pub name: String,
    kind: Kind,
    db: PathBuf,
}

#[derive(Debug, Clone)]
enum Kind {
    /// The keychain entry holding the browser's cookie key.
    Chromium {
        service: &'static str,
        account: &'static str,
    },
    Firefox,
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceView {
    pub id: String,
    pub name: String,
}

impl From<&Source> for SourceView {
    fn from(s: &Source) -> Self {
        Self {
            id: s.id.clone(),
            name: s.name.clone(),
        }
    }
}

/// A site with cookies in a source, e.g. `github.com` for `.github.com`
/// and `gist.github.com`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Site {
    pub site: String,
    pub cookies: usize,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SameSite {
    None,
    Lax,
    Strict,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Cookie {
    pub name: String,
    pub value: String,
    /// As the browser keeps it: a leading dot for a domain cookie, none for
    /// a host-only one.
    pub domain: String,
    pub path: String,
    /// Unix seconds; `None` for a session cookie.
    pub expires: Option<i64>,
    pub secure: bool,
    pub http_only: bool,
    pub same_site: Option<SameSite>,
}

/// The site a cookie host belongs to: its registrable domain.
pub fn site_of(host: &str) -> String {
    let host = host.trim_start_matches('.').to_ascii_lowercase();
    psl::domain_str(&host).unwrap_or(&host).to_string()
}

/// The browser profiles on this computer with a cookie store.
pub fn sources(home: &Path) -> Vec<Source> {
    let mut out = Vec::new();
    #[cfg(target_os = "macos")]
    {
        let support = home.join("Library/Application Support");
        for (name, dir, service, account) in CHROMIUM {
            chromium_profiles(&support.join(dir), name, service, account, &mut out);
        }
        firefox_profiles(&support.join("Firefox"), &mut out);
    }
    #[cfg(windows)]
    if let Some(appdata) = std::env::var_os("APPDATA") {
        firefox_profiles(&PathBuf::from(appdata).join("Mozilla/Firefox"), &mut out);
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    firefox_profiles(&home.join(".mozilla/firefox"), &mut out);
    let _ = home;
    out
}

/// Name, folder under Application Support, and keychain service/account.
#[cfg(target_os = "macos")]
const CHROMIUM: &[(&str, &str, &str, &str)] = &[
    ("Chrome", "Google/Chrome", "Chrome Safe Storage", "Chrome"),
    (
        "Chrome Beta",
        "Google/Chrome Beta",
        "Chrome Safe Storage",
        "Chrome",
    ),
    ("Arc", "Arc/User Data", "Arc Safe Storage", "Arc"),
    (
        "Brave",
        "BraveSoftware/Brave-Browser",
        "Brave Safe Storage",
        "Brave",
    ),
    (
        "Edge",
        "Microsoft Edge",
        "Microsoft Edge Safe Storage",
        "Microsoft Edge",
    ),
    ("Vivaldi", "Vivaldi", "Vivaldi Safe Storage", "Vivaldi"),
    ("Chromium", "Chromium", "Chromium Safe Storage", "Chromium"),
];

#[cfg(target_os = "macos")]
fn chromium_profiles(
    root: &Path,
    browser: &str,
    service: &'static str,
    account: &'static str,
    out: &mut Vec<Source>,
) {
    let Ok(dirs) = std::fs::read_dir(root) else {
        return;
    };
    // Profile names the person chose, from "Local State".
    let names: BTreeMap<String, String> = std::fs::read(root.join("Local State"))
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .and_then(|v| {
            v.pointer("/profile/info_cache")?.as_object().map(|m| {
                m.iter()
                    .filter_map(|(k, v)| Some((k.clone(), v.get("name")?.as_str()?.to_string())))
                    .collect()
            })
        })
        .unwrap_or_default();
    let mut found: Vec<(String, PathBuf)> = dirs
        .flatten()
        .filter_map(|d| {
            let dir = d.file_name().to_string_lossy().to_string();
            if dir != "Default" && !dir.starts_with("Profile ") {
                return None;
            }
            let p = d.path();
            [p.join("Cookies"), p.join("Network/Cookies")]
                .into_iter()
                .find(|c| c.is_file())
                .map(|db| (dir, db))
        })
        .collect();
    found.sort();
    let many = found.len() > 1;
    for (dir, db) in found {
        let name = if many {
            format!("{browser} ({})", names.get(&dir).unwrap_or(&dir))
        } else {
            browser.to_string()
        };
        out.push(Source {
            id: db.to_string_lossy().into_owned(),
            name,
            kind: Kind::Chromium { service, account },
            db,
        });
    }
}

fn firefox_profiles(root: &Path, out: &mut Vec<Source>) {
    let Ok(dirs) = std::fs::read_dir(root.join("Profiles")) else {
        return;
    };
    let mut found: Vec<(String, PathBuf)> = dirs
        .flatten()
        .filter_map(|d| {
            let db = d.path().join("cookies.sqlite");
            db.is_file().then(|| {
                // "abcd1234.default-release" → "default-release"
                let dir = d.file_name().to_string_lossy().to_string();
                (
                    dir.split_once('.')
                        .map_or(dir.clone(), |(_, n)| n.to_string()),
                    db,
                )
            })
        })
        .collect();
    found.sort();
    let many = found.len() > 1;
    for (profile, db) in found {
        out.push(Source {
            id: db.to_string_lossy().into_owned(),
            name: if many {
                format!("Firefox ({profile})")
            } else {
                "Firefox".into()
            },
            kind: Kind::Firefox,
            db,
        });
    }
}

/// The sites a source has cookies for, most cookies first. Reads hosts only,
/// so it doesn't need the keychain.
pub fn sites(src: &Source, now: i64) -> anyhow::Result<Vec<Site>> {
    let mut by_site: BTreeMap<String, usize> = BTreeMap::new();
    for row in rows(src, now)? {
        *by_site.entry(site_of(&row.domain)).or_default() += 1;
    }
    let mut out: Vec<Site> = by_site
        .into_iter()
        .map(|(site, cookies)| Site { site, cookies })
        .collect();
    out.sort_by(|a, b| b.cookies.cmp(&a.cookies).then_with(|| a.site.cmp(&b.site)));
    Ok(out)
}

/// The unexpired cookies for `sites`, decrypted. For a Chrome-family
/// browser this reads its key from the keychain, which macOS asks about.
pub fn cookies(src: &Source, sites: &[String], now: i64) -> anyhow::Result<Vec<Cookie>> {
    let want: HashSet<&str> = sites.iter().map(String::as_str).collect();
    let rows: Vec<Row> = rows(src, now)?
        .into_iter()
        .filter(|r| want.contains(site_of(&r.domain).as_str()))
        .collect();
    let key = match src.kind {
        Kind::Chromium { service, account } if rows.iter().any(|r| !r.sealed.is_empty()) => {
            Some(chromium_key(&keychain_password(service, account)?))
        }
        _ => None,
    };
    let version = match src.kind {
        Kind::Chromium { .. } => chromium_version(&src.db).unwrap_or(0),
        Kind::Firefox => 0,
    };
    let mut out = Vec::with_capacity(rows.len());
    let mut unreadable = 0;
    for r in rows {
        let value = if r.sealed.is_empty() {
            r.value
        } else {
            match key
                .as_ref()
                .and_then(|k| chromium_open(k, &r.sealed, &r.domain, version))
            {
                Some(v) => v,
                None => {
                    unreadable += 1;
                    continue;
                }
            }
        };
        out.push(Cookie {
            name: r.name,
            value,
            domain: r.domain,
            path: r.path,
            expires: r.expires,
            secure: r.secure,
            http_only: r.http_only,
            same_site: r.same_site,
        });
    }
    if unreadable > 0 {
        tracing::warn!(unreadable, source = %src.name, "couldn't decrypt some cookies");
    }
    Ok(out)
}

/// A cookie as stored, its value maybe still sealed.
struct Row {
    name: String,
    value: String,
    sealed: Vec<u8>,
    domain: String,
    path: String,
    expires: Option<i64>,
    secure: bool,
    http_only: bool,
    same_site: Option<SameSite>,
}

/// Seconds between 1601-01-01 (Chrome's epoch) and 1970-01-01.
const WINDOWS_EPOCH: i64 = 11_644_473_600;

fn rows(src: &Source, now: i64) -> anyhow::Result<Vec<Row>> {
    let copy = Snapshot::of(&src.db)?;
    let db = rusqlite::Connection::open(&copy.db)?;
    let mut out = Vec::new();
    match src.kind {
        Kind::Chromium { .. } => {
            let mut q = db.prepare(
                "SELECT host_key, name, value, encrypted_value, path, expires_utc, \
                 is_secure, is_httponly, samesite FROM cookies",
            )?;
            let mut it = q.query([])?;
            while let Some(r) = it.next()? {
                let expires_utc: i64 = r.get(5)?;
                let expires = (expires_utc > 0).then(|| expires_utc / 1_000_000 - WINDOWS_EPOCH);
                out.push(Row {
                    domain: r.get(0)?,
                    name: r.get(1)?,
                    value: r.get(2)?,
                    sealed: r.get(3)?,
                    path: r.get(4)?,
                    expires,
                    secure: r.get::<_, i64>(6)? != 0,
                    http_only: r.get::<_, i64>(7)? != 0,
                    same_site: match r.get::<_, i64>(8)? {
                        0 => Some(SameSite::None),
                        1 => Some(SameSite::Lax),
                        2 => Some(SameSite::Strict),
                        _ => None,
                    },
                });
            }
        }
        Kind::Firefox => {
            // Containers and private windows have their own cookies
            // (originAttributes); only the plain profile's are imported.
            let mut q = db.prepare(
                "SELECT host, name, value, path, expiry, isSecure, isHttpOnly, sameSite \
                 FROM moz_cookies WHERE originAttributes = ''",
            )?;
            let mut it = q.query([])?;
            while let Some(r) = it.next()? {
                let expiry: i64 = r.get(4)?;
                // Newer Firefox keeps milliseconds.
                let expires = if expiry > 100_000_000_000 {
                    expiry / 1000
                } else {
                    expiry
                };
                out.push(Row {
                    domain: r.get(0)?,
                    name: r.get(1)?,
                    value: r.get(2)?,
                    sealed: Vec::new(),
                    path: r.get(3)?,
                    expires: (expires > 0).then_some(expires),
                    secure: r.get::<_, i64>(5)? != 0,
                    http_only: r.get::<_, i64>(6)? != 0,
                    same_site: match r.get::<_, i64>(7)? {
                        0 => Some(SameSite::None),
                        1 => Some(SameSite::Lax),
                        2 => Some(SameSite::Strict),
                        _ => None,
                    },
                });
            }
        }
    }
    out.retain(|r| r.expires.is_none_or(|e| e > now) && !r.domain.is_empty());
    Ok(out)
}

/// A copy of a browser's cookie database (and its write-ahead log), so a
/// running browser's lock doesn't get in the way and its file isn't touched.
struct Snapshot {
    dir: PathBuf,
    db: PathBuf,
}

impl Snapshot {
    fn of(db: &Path) -> anyhow::Result<Self> {
        let dir = std::env::temp_dir().join(format!("tab-cookies-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir)?;
        let copy = Self {
            db: dir.join("cookies.sqlite"),
            dir,
        };
        std::fs::copy(db, &copy.db)?;
        let wal = PathBuf::from(format!("{}-wal", db.display()));
        if wal.is_file() {
            std::fs::copy(&wal, format!("{}-wal", copy.db.display()))?;
        }
        Ok(copy)
    }
}

impl Drop for Snapshot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn chromium_version(db: &Path) -> anyhow::Result<i64> {
    let copy = Snapshot::of(db)?;
    let db = rusqlite::Connection::open(&copy.db)?;
    let v: String = db.query_row("SELECT value FROM meta WHERE key = 'version'", [], |r| {
        r.get(0)
    })?;
    Ok(v.parse()?)
}

#[cfg(target_os = "macos")]
fn keychain_password(service: &str, account: &str) -> anyhow::Result<String> {
    keyring::Entry::new(service, account)?
        .get_password()
        .map_err(|e| anyhow::anyhow!("couldn't read the browser's key from the keychain: {e}"))
}

#[cfg(not(target_os = "macos"))]
fn keychain_password(_service: &str, _account: &str) -> anyhow::Result<String> {
    anyhow::bail!("this browser's cookies can only be read on macOS")
}

/// Chrome on macOS: AES-128 key from the keychain password.
fn chromium_key(password: &str) -> [u8; 16] {
    let mut key = [0u8; 16];
    ring::pbkdf2::derive(
        ring::pbkdf2::PBKDF2_HMAC_SHA1,
        std::num::NonZeroU32::new(1003).unwrap(),
        b"saltysalt",
        password.as_bytes(),
        &mut key,
    );
    key
}

/// Opens a `v10` value. Since database version 24 the plaintext starts with
/// the SHA-256 of the cookie's host, which binds it to that host.
fn chromium_open(key: &[u8; 16], sealed: &[u8], host: &str, version: i64) -> Option<String> {
    use aes::cipher::{BlockDecryptMut, KeyIvInit, block_padding::Pkcs7};
    use sha2::Digest;
    let body = sealed.strip_prefix(b"v10")?;
    let plain = cbc::Decryptor::<aes::Aes128>::new(key.into(), &[b' '; 16].into())
        .decrypt_padded_vec_mut::<Pkcs7>(body)
        .ok()?;
    let plain = if version >= 24 {
        let (hash, rest) = plain.split_at_checked(32)?;
        if hash != sha2::Sha256::digest(host.as_bytes()).as_slice() {
            return None;
        }
        rest.to_vec()
    } else {
        plain
    };
    String::from_utf8(plain).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seal(key: &[u8; 16], plain: &[u8]) -> Vec<u8> {
        use aes::cipher::{BlockEncryptMut, KeyIvInit, block_padding::Pkcs7};
        let mut out = b"v10".to_vec();
        out.extend(
            cbc::Encryptor::<aes::Aes128>::new(key.into(), &[b' '; 16].into())
                .encrypt_padded_vec_mut::<Pkcs7>(plain),
        );
        out
    }

    #[test]
    fn opens_chrome_values_old_and_new() {
        use sha2::Digest;
        let key = chromium_key("hunter2");
        assert_eq!(
            chromium_open(&key, &seal(&key, b"abc"), ".github.com", 23).as_deref(),
            Some("abc")
        );

        let mut bound = sha2::Sha256::digest(b".github.com").to_vec();
        bound.extend(b"session");
        let sealed = seal(&key, &bound);
        assert_eq!(
            chromium_open(&key, &sealed, ".github.com", 24).as_deref(),
            Some("session")
        );
        // Bound to its host: moved to another, it doesn't open.
        assert_eq!(chromium_open(&key, &sealed, ".evil.com", 24), None);
        assert_eq!(
            chromium_open(&chromium_key("wrong"), &sealed, ".github.com", 24),
            None
        );
    }

    #[test]
    fn groups_hosts_by_site() {
        assert_eq!(site_of(".github.com"), "github.com");
        assert_eq!(site_of("gist.github.com"), "github.com");
        assert_eq!(site_of("foo.github.io"), "foo.github.io");
        assert_eq!(site_of("www.bbc.co.uk"), "bbc.co.uk");
        assert_eq!(site_of("localhost"), "localhost");
    }

    #[test]
    fn reads_firefox_profiles() {
        let root = std::env::temp_dir().join(format!("tab-firefox-{}", uuid::Uuid::new_v4()));
        let prof = root.join("Profiles/ab12.default-release");
        std::fs::create_dir_all(&prof).unwrap();
        let db = rusqlite::Connection::open(prof.join("cookies.sqlite")).unwrap();
        db.execute_batch(
            "CREATE TABLE moz_cookies (host TEXT, name TEXT, value TEXT, path TEXT, expiry INTEGER, \
             isSecure INTEGER, isHttpOnly INTEGER, sameSite INTEGER, originAttributes TEXT);
             INSERT INTO moz_cookies VALUES ('.github.com','user_session','s1','/',4000000000000,1,1,1,'');
             INSERT INTO moz_cookies VALUES ('gist.github.com','g','s2','/',4000000000,1,0,0,'');
             INSERT INTO moz_cookies VALUES ('.github.com','old','x','/',1000,1,0,0,'');
             INSERT INTO moz_cookies VALUES ('.github.com','box','x','/',4000000000,1,0,0,'^userContextId=1');
             INSERT INTO moz_cookies VALUES ('example.org','e','x','/',4000000000,0,0,2,'');",
        )
        .unwrap();
        drop(db);

        let mut found = Vec::new();
        firefox_profiles(&root, &mut found);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "Firefox");

        let now = 1_800_000_000;
        assert_eq!(
            sites(&found[0], now).unwrap(),
            vec![
                Site {
                    site: "github.com".into(),
                    cookies: 2
                },
                Site {
                    site: "example.org".into(),
                    cookies: 1
                },
            ]
        );
        let got = cookies(&found[0], &["github.com".into()], now).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].name, "user_session");
        assert_eq!(got[0].expires, Some(4_000_000_000));
        assert_eq!(got[0].same_site, Some(SameSite::Lax));
        assert!(got[0].http_only);
        let _ = std::fs::remove_dir_all(&root);
    }
}
