//! The manifest marketplace: an index of signed manifests people can
//! install (see `docs/recipe-manifests.md`).
//!
//! The index is a JSON file (`marketplace/index.json` in tab's repo by
//! default). Each listing names a manifest file beside it, its SHA-256 and
//! an Ed25519 signature over the file's bytes. tab checks both against the
//! key this build was made with (`TAB_MARKETPLACE_PUBKEY`, base64), then
//! validates the manifest, before showing what it would call and installing
//! it into `<recipes>/marketplace/<id>.json`. A build without a key has no
//! marketplace. Manifests from the marketplace can't run plugins, and like
//! any manifest that isn't built in, may only call read-only tools.

use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::manifest::{MARKETPLACE_DIR, Manifest, Source};

/// Where the index is, unless the build says otherwise
/// (`TAB_MARKETPLACE_INDEX`).
pub const DEFAULT_INDEX: &str =
    "https://raw.githubusercontent.com/VincentCosta6/tab/main/marketplace/index.json";

/// The index format this tab reads.
pub const INDEX_VERSION: u32 = 1;

/// Largest manifest tab downloads.
const MAX_BYTES: usize = 256 * 1024;

pub fn index_url() -> &'static str {
    option_env!("TAB_MARKETPLACE_INDEX").unwrap_or(DEFAULT_INDEX)
}

/// The key listings are signed with, if this build has one.
pub fn public_key() -> Option<Vec<u8>> {
    let k = option_env!("TAB_MARKETPLACE_PUBKEY")?.trim();
    base64::engine::general_purpose::STANDARD.decode(k).ok()
}

#[derive(Debug, Clone, Deserialize)]
pub struct Index {
    pub version: u32,
    #[serde(default)]
    pub manifests: Vec<Listing>,
}

/// One manifest on offer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Listing {
    pub id: String,
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub author: Option<String>,
    pub version: String,
    /// The manifest file, from the index's folder (or a full URL).
    pub file: String,
    /// Hex SHA-256 of the file.
    pub sha256: String,
    /// Base64 Ed25519 signature over the file's bytes.
    pub signature: String,
}

/// A listing, with what's installed.
#[derive(Debug, Clone, Serialize)]
pub struct ListingView {
    #[serde(flatten)]
    pub listing: Listing,
    /// The installed version, if it's installed.
    pub installed: Option<String>,
}

/// What installing a listing would do, for the person to confirm.
#[derive(Debug, Clone, Serialize)]
pub struct Preview {
    pub listing: Listing,
    /// Tools it calls on the connection, in order.
    pub calls: Vec<String>,
    pub fills_queue: bool,
    /// It takes the place of a built-in.
    pub replaces_built_in: bool,
    /// The service's MCP server.
    pub url: String,
}

/// Checks a manifest file against its listing: size, hash, signature, and
/// that it's a valid manifest with the listing's id and no plugin.
pub fn verify(listing: &Listing, bytes: &[u8], key: &[u8]) -> anyhow::Result<Manifest> {
    anyhow::ensure!(bytes.len() <= MAX_BYTES, "the manifest is too big");
    let hash = hex(&Sha256::digest(bytes));
    anyhow::ensure!(
        hash.eq_ignore_ascii_case(listing.sha256.trim()),
        "the manifest doesn't match its listing (SHA-256)"
    );
    let sig = base64::engine::general_purpose::STANDARD
        .decode(listing.signature.trim())
        .map_err(|_| anyhow::anyhow!("the listing's signature isn't base64"))?;
    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, key)
        .verify(bytes, &sig)
        .map_err(|_| anyhow::anyhow!("the manifest isn't signed by tab's marketplace key"))?;
    let json = std::str::from_utf8(bytes)?;
    let m = Manifest::parse(json).map_err(|e| anyhow::anyhow!("{e}"))?;
    anyhow::ensure!(
        m.id == listing.id,
        "the manifest's id ({}) isn't the listing's ({})",
        m.id,
        listing.id
    );
    anyhow::ensure!(
        m.queue.as_ref().is_none_or(|q| q.plugin.is_none()),
        "manifests from the marketplace can't run plugins"
    );
    Ok(m)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// What a manifest would call, for the preview.
pub fn calls(m: &Manifest) -> Vec<String> {
    let Some(q) = &m.queue else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    let tools = q
        .steps
        .iter()
        .flat_map(|s| s.call.iter())
        .chain(q.check.iter().flat_map(|c| c.call.iter()));
    for t in tools {
        if !out.contains(t) {
            out.push(t.clone());
        }
    }
    out
}

fn client() -> anyhow::Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()?)
}

fn file_url(index: &str, file: &str) -> anyhow::Result<String> {
    if file.starts_with("https://") {
        return Ok(file.to_string());
    }
    anyhow::ensure!(
        !file.contains("..") && !file.starts_with('/') && !file.contains("://"),
        "the listing's file \"{file}\" isn't a file beside the index"
    );
    let base = index.rsplit_once('/').map_or(index, |(b, _)| b);
    Ok(format!("{base}/{file}"))
}

/// The listings on offer, and which are installed.
pub async fn listings(recipes: &Path) -> anyhow::Result<Vec<ListingView>> {
    anyhow::ensure!(
        public_key().is_some(),
        "this build of tab has no marketplace key"
    );
    let index: Index = client()?
        .get(index_url())
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    anyhow::ensure!(
        index.version == INDEX_VERSION,
        "the marketplace index is version {}, and this tab reads {INDEX_VERSION}",
        index.version
    );
    Ok(index
        .manifests
        .into_iter()
        .map(|listing| ListingView {
            installed: installed_version(recipes, &listing.id),
            listing,
        })
        .collect())
}

async fn fetch(listing: &Listing) -> anyhow::Result<(Vec<u8>, Manifest)> {
    let key =
        public_key().ok_or_else(|| anyhow::anyhow!("this build of tab has no marketplace key"))?;
    let url = file_url(index_url(), &listing.file)?;
    let bytes = client()?
        .get(&url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?
        .to_vec();
    let m = verify(listing, &bytes, &key)?;
    Ok((bytes, m))
}

/// Downloads and checks a listing, and says what it would do.
pub async fn preview(listing: &Listing) -> anyhow::Result<Preview> {
    let (_, m) = fetch(listing).await?;
    Ok(Preview {
        listing: listing.clone(),
        calls: calls(&m),
        fills_queue: m.queue.is_some(),
        replaces_built_in: super::manifest::status()
            .manifests
            .iter()
            .any(|i| i.id == m.id && i.source == Source::BuiltIn),
        url: m.url.clone(),
    })
}

fn path_for(recipes: &Path, id: &str) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(super::valid_id(id), "\"{id}\" can't be an id");
    Ok(recipes.join(MARKETPLACE_DIR).join(format!("{id}.json")))
}

fn installed_version(recipes: &Path, id: &str) -> Option<String> {
    let json = std::fs::read_to_string(path_for(recipes, id).ok()?).ok()?;
    let v: serde_json::Value = serde_json::from_str(&json).ok()?;
    Some(v["version"].as_str().unwrap_or("installed").to_string())
}

/// Downloads, checks and saves a listing. Load the folder again after.
pub async fn install(recipes: &Path, listing: &Listing) -> anyhow::Result<()> {
    let (bytes, _) = fetch(listing).await?;
    save(recipes, &listing.id, &bytes)
}

fn save(recipes: &Path, id: &str, bytes: &[u8]) -> anyhow::Result<()> {
    let path = path_for(recipes, id)?;
    std::fs::create_dir_all(path.parent().expect("has a folder"))?;
    let tmp = path.with_extension("json.part");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Removes an installed listing. Load the folder again after.
pub fn uninstall(recipes: &Path, id: &str) -> anyhow::Result<()> {
    let path = path_for(recipes, id)?;
    match std::fs::remove_file(&path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

/// Signs a manifest file for the index: its SHA-256 and signature, with a
/// PKCS#8 Ed25519 key (`examples/manifest_sign.rs`).
pub fn sign(bytes: &[u8], pkcs8: &[u8]) -> anyhow::Result<(String, String)> {
    let pair = ring::signature::Ed25519KeyPair::from_pkcs8(pkcs8)
        .map_err(|_| anyhow::anyhow!("that isn't a PKCS#8 Ed25519 key"))?;
    let sig = pair.sign(bytes);
    Ok((
        hex(&Sha256::digest(bytes)),
        base64::engine::general_purpose::STANDARD.encode(sig.as_ref()),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::KeyPair;

    fn key() -> (Vec<u8>, Vec<u8>) {
        let rng = ring::rand::SystemRandom::new();
        let pkcs8 = ring::signature::Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        let pair = ring::signature::Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        (pkcs8.as_ref().to_vec(), pair.public_key().as_ref().to_vec())
    }

    fn manifest(id: &str, plugin: bool) -> Vec<u8> {
        let queue = if plugin {
            serde_json::json!({ "description": "d", "kinds": [{ "kind": "x.y", "label": "Y", "icon": "y" }],
                "plugin": { "command": "node", "args": ["p.js"] } })
        } else {
            serde_json::json!({ "description": "d", "kinds": [{ "kind": "x.y", "label": "Y", "icon": "y" }],
                "steps": [{ "call": ["list_things"] }],
                "item": { "id": "{id}", "title": "{title}" },
                "check": { "call": ["get_thing"], "args": { "id": "{$external_id}" }, "known": "{id}" } })
        };
        serde_json::json!({ "manifest": 1, "id": id, "name": "Thing", "url": "https://mcp.thing.dev",
            "description": "Agents can read things.", "version": "1.2.0", "queue": queue })
        .to_string()
        .into_bytes()
    }

    fn listing(id: &str, bytes: &[u8], pkcs8: &[u8]) -> Listing {
        let (sha256, signature) = sign(bytes, pkcs8).unwrap();
        Listing {
            id: id.into(),
            name: "Thing".into(),
            description: "d".into(),
            author: None,
            version: "1.2.0".into(),
            file: format!("{id}.json"),
            sha256,
            signature,
        }
    }

    #[test]
    fn signed_manifests_verify_and_tampered_ones_dont() {
        let (pkcs8, public) = key();
        let bytes = manifest("thing", false);
        let l = listing("thing", &bytes, &pkcs8);
        let m = verify(&l, &bytes, &public).unwrap();
        assert_eq!(calls(&m), ["list_things", "get_thing"]);

        let mut changed = bytes.clone();
        changed.extend_from_slice(b" ");
        assert!(
            verify(&l, &changed, &public)
                .unwrap_err()
                .to_string()
                .contains("SHA-256")
        );
        // Right hash, someone else's key.
        let (other, _) = key();
        let forged = listing("thing", &bytes, &other);
        assert!(
            verify(&forged, &bytes, &public)
                .unwrap_err()
                .to_string()
                .contains("signed")
        );
        // A listing can't install a manifest under another id.
        let mut wrong = l.clone();
        wrong.id = "other".into();
        assert!(
            verify(&wrong, &bytes, &public)
                .unwrap_err()
                .to_string()
                .contains("id")
        );
        // No plugins from the marketplace.
        let p = manifest("thing", true);
        assert!(
            verify(&listing("thing", &p, &pkcs8), &p, &public)
                .unwrap_err()
                .to_string()
                .contains("plugins")
        );
    }

    #[test]
    fn files_stay_beside_the_index() {
        let idx = "https://example.com/m/index.json";
        assert_eq!(
            file_url(idx, "a.json").unwrap(),
            "https://example.com/m/a.json"
        );
        assert_eq!(
            file_url(idx, "https://cdn.dev/a.json").unwrap(),
            "https://cdn.dev/a.json"
        );
        assert!(file_url(idx, "../a.json").is_err());
        assert!(file_url(idx, "/a.json").is_err());
        assert!(file_url(idx, "http://evil/a.json").is_err());
    }

    #[test]
    fn installs_and_removes_files() {
        let dir = std::env::temp_dir().join(format!("tab-market-{}", uuid::Uuid::new_v4()));
        save(&dir, "thing", &manifest("thing", false)).unwrap();
        assert_eq!(installed_version(&dir, "thing").as_deref(), Some("1.2.0"));
        assert!(save(&dir, "../x", b"{}").is_err());
        uninstall(&dir, "thing").unwrap();
        assert_eq!(installed_version(&dir, "thing"), None);
        uninstall(&dir, "thing").unwrap();
    }

    #[test]
    fn the_repos_index_reads() {
        let index: Index =
            serde_json::from_str(include_str!("../../../../marketplace/index.json")).unwrap();
        assert_eq!(index.version, INDEX_VERSION);
    }
}
