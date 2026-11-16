//! Signs manifests for the marketplace (`marketplace/`, see its README).
//!
//! cargo run -p tab-core --example manifest_sign -- keygen > key.pk8.b64
//!     A new key: prints the private key (base64 PKCS#8) on stdout, which
//!     you keep secret, and the public key on stderr, which builds get as
//!     TAB_MARKETPLACE_PUBKEY.
//! TAB_MARKETPLACE_KEY=$(cat key.pk8.b64) cargo run -p tab-core --example manifest_sign -- index marketplace
//!     Signs every manifest in marketplace/manifests and writes
//!     marketplace/index.json.

use base64::Engine;
use ring::signature::KeyPair;
use tab_core::remote_mcp::manifest::Manifest;
use tab_core::remote_mcp::marketplace::{INDEX_VERSION, Listing, sign};

fn main() -> anyhow::Result<()> {
    let b64 = base64::engine::general_purpose::STANDARD;
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("keygen") => {
            let rng = ring::rand::SystemRandom::new();
            let pkcs8 = ring::signature::Ed25519KeyPair::generate_pkcs8(&rng)
                .map_err(|_| anyhow::anyhow!("couldn't make a key"))?;
            let pair = ring::signature::Ed25519KeyPair::from_pkcs8(pkcs8.as_ref())
                .map_err(|_| anyhow::anyhow!("couldn't read the key"))?;
            println!("{}", b64.encode(pkcs8.as_ref()));
            eprintln!(
                "TAB_MARKETPLACE_PUBKEY={}",
                b64.encode(pair.public_key().as_ref())
            );
        }
        Some("index") => {
            let root = std::path::PathBuf::from(args.get(1).map_or("marketplace", String::as_str));
            let key = b64.decode(std::env::var("TAB_MARKETPLACE_KEY")?.trim())?;
            let mut paths: Vec<_> = std::fs::read_dir(root.join("manifests"))?
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "json"))
                .collect();
            paths.sort();
            let mut manifests = Vec::new();
            for path in paths {
                let bytes = std::fs::read(&path)?;
                let m = Manifest::parse(std::str::from_utf8(&bytes)?)
                    .map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
                anyhow::ensure!(
                    m.queue.as_ref().is_none_or(|q| q.plugin.is_none()),
                    "{}: marketplace manifests can't run plugins",
                    path.display()
                );
                let (sha256, signature) = sign(&bytes, &key)?;
                manifests.push(Listing {
                    id: m.id.clone(),
                    name: m.name.clone(),
                    description: m
                        .queue
                        .as_ref()
                        .map_or(m.description.clone(), |q| q.description.clone()),
                    author: m.author.clone(),
                    version: m.version.clone().unwrap_or_else(|| "1".into()),
                    file: format!("manifests/{}", path.file_name().unwrap().to_string_lossy()),
                    sha256,
                    signature,
                });
            }
            let index = serde_json::json!({ "version": INDEX_VERSION, "manifests": manifests });
            std::fs::write(
                root.join("index.json"),
                serde_json::to_string_pretty(&index)? + "\n",
            )?;
            eprintln!("signed {} manifests", manifests.len());
        }
        _ => anyhow::bail!("usage: manifest_sign keygen | index [marketplace folder]"),
    }
    Ok(())
}
