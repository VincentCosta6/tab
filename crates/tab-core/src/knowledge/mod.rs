//! Indexes what projects know so agents can search it: each resource's
//! contents (a website's pages, a file's text, the email under a label or
//! from a sender, a note) and each repo's docs from its checkout here.
//!
//! Everything is read and indexed on this device and stays here (see
//! `store::knowledge`); other devices only see a resource's status and how
//! many pieces it has. Pieces go into the same search as items: keyword
//! search right away, search by meaning once the embedding model
//! (`crate::embed`) picks them up.

pub mod chunk;
pub mod extract;
pub mod repo;
pub mod web;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};

use crate::config::ConnectorConfig;
use crate::gateway::Gateway;
use crate::store::{IndexTarget, Store};
use chunk::Doc;

/// What the indexer reads through the app: hub files and the email connection.
#[async_trait::async_trait]
pub trait Sources: Send + Sync {
    /// A file's bytes from the hub, by hash (fetched from tab cloud if need be).
    async fn file(&self, hash: &str) -> anyhow::Result<Option<Vec<u8>>>;
    /// The email connection's settings, when one is on.
    async fn email(&self) -> Option<ConnectorConfig>;
}

/// Most messages kept from one email resource.
const MAX_EMAILS: usize = 200;

/// Why a read failed, and what that means for the resource.
enum Failure {
    /// Needs the person: connect email, sign in.
    SignIn(String),
    Failed(anyhow::Error),
}

impl From<anyhow::Error> for Failure {
    fn from(e: anyhow::Error) -> Self {
        Self::Failed(e)
    }
}

/// When to look again, from a website's `schedule` (hourly, daily, weekly)
/// or the kind's default. Files and notes change only when edited.
fn next_look(t: &IndexTarget, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let hours = match t.kind.as_str() {
        "website" => match t.options["schedule"].as_str() {
            Some("hourly") => 1,
            Some("weekly") => 24 * 7,
            _ => 24,
        },
        "email" => 1,
        "repo" => 6,
        _ => return None,
    };
    Some(now + chrono::Duration::hours(hours))
}

/// Reads a resource's documents.
async fn read(gw: &Gateway, sources: &dyn Sources, t: &IndexTarget) -> Result<Vec<Doc>, Failure> {
    let url = t.url.as_deref().unwrap_or_default();
    Ok(match t.kind.as_str() {
        "note" => vec![Doc {
            title: t.title.clone(),
            source: None,
            text: t.note.clone(),
        }],
        "website" => {
            let depth = t.options["depth"].as_u64().unwrap_or(1).min(2) as u32;
            web::crawl(gw, url, depth).await?
        }
        "file" => {
            let name = t.options["name"].as_str().unwrap_or(&t.title).to_string();
            let bytes = sources
                .file(url)
                .await?
                .ok_or_else(|| anyhow::anyhow!("the hub doesn't have this file any more"))?;
            let text = tokio::task::spawn_blocking(move || extract::file_text(&name, &bytes))
                .await
                .map_err(|e| anyhow::anyhow!("reading the file stopped: {e}"))??;
            vec![Doc {
                title: t.title.clone(),
                source: None,
                text,
            }]
        }
        "email" => {
            let Some(cfg) = sources.email().await else {
                return Err(Failure::SignIn("Connect email so tab can read it".into()));
            };
            crate::connectors::email::read_selection(&cfg, url, MAX_EMAILS).await?
        }
        "repo" => {
            let root = t.path.clone().unwrap_or_default();
            if !Path::new(&root).is_dir() {
                // Checked out on another computer: nothing to read here.
                return Ok(Vec::new());
            }
            let repo = t.title.clone();
            tokio::task::spawn_blocking(move || {
                let root = Path::new(&root);
                repo::read_docs(root, &repo, &repo::doc_files(root))
            })
            .await
            .map_err(|e| anyhow::anyhow!("reading the repo stopped: {e}"))?
        }
        k => {
            return Err(Failure::Failed(anyhow::anyhow!(
                "{k} resources aren't indexed"
            )));
        }
    })
}

fn fingerprint(docs: &[Doc]) -> String {
    let mut h = Sha256::new();
    for d in docs {
        for part in [
            d.title.as_str(),
            d.source.as_deref().unwrap_or(""),
            d.text.as_str(),
        ] {
            h.update(part.as_bytes());
            h.update([0]);
        }
    }
    format!("{:x}", h.finalize())
}

/// Reads and indexes one resource. Errors are recorded on it, not returned.
pub async fn index_one(
    store: &Store,
    gw: &Gateway,
    sources: &dyn Sources,
    t: &IndexTarget,
) -> anyhow::Result<()> {
    let now = Utc::now();
    let next = next_look(t, now);
    match read(gw, sources, t).await {
        Ok(docs) => {
            let fp = fingerprint(&docs);
            if store.knowledge_fingerprint(&t.id)?.as_deref() == Some(fp.as_str()) {
                store.knowledge_unchanged(t, next, now)?;
            } else {
                let pieces = chunk::chunk(&docs);
                tracing::info!(resource = %t.id, kind = %t.kind, docs = docs.len(), pieces = pieces.len(), "indexed");
                store.knowledge_put(t, &fp, &pieces, next, now)?;
            }
        }
        Err(Failure::SignIn(why)) => {
            store.knowledge_failed(
                t,
                "needs_sign_in",
                &why,
                now + chrono::Duration::minutes(30),
            )?;
        }
        Err(Failure::Failed(e)) => {
            tracing::warn!(resource = %t.id, kind = %t.kind, error = %e, "couldn't index");
            let retry = next
                .unwrap_or(now + chrono::Duration::hours(6))
                .min(now + chrono::Duration::hours(6));
            store.knowledge_failed(t, "failed", &format!("{e:#}"), retry)?;
        }
    }
    Ok(())
}

/// Keeps project resources indexed: whatever is new, edited or due, then
/// waits for a change (or a minute).
pub fn spawn(store: Arc<Store>, gw: Arc<Gateway>, sources: Arc<dyn Sources>) {
    tokio::spawn(async move {
        let wake = store.knowledge_wake();
        // Let launch settle first.
        tokio::time::sleep(Duration::from_secs(5)).await;
        loop {
            match store.knowledge_due(Utc::now(), 20) {
                Ok(due) => {
                    for t in &due {
                        if let Err(e) = index_one(&store, &gw, sources.as_ref(), t).await {
                            tracing::warn!(resource = %t.id, error = %e, "indexing failed");
                        }
                    }
                    if !due.is_empty() {
                        continue;
                    }
                }
                Err(e) => tracing::warn!(error = %e, "couldn't list resources to index"),
            }
            tokio::select! {
                _ = wake.notified() => {}
                _ = tokio::time::sleep(Duration::from_secs(60)) => {}
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Reader, ResourceInput};

    struct NoSources;

    #[async_trait::async_trait]
    impl Sources for NoSources {
        async fn file(&self, hash: &str) -> anyhow::Result<Option<Vec<u8>>> {
            Ok((hash == "sha256:md")
                .then(|| b"# Onboarding\nShip on Fridays only with a rollback plan.".to_vec()))
        }
        async fn email(&self) -> Option<ConnectorConfig> {
            None
        }
    }

    #[tokio::test]
    async fn indexes_notes_files_and_reports_what_needs_the_person() {
        let store = Arc::new(Store::in_memory().unwrap());
        let gw = Gateway::new(store.clone());
        let now = Utc::now();
        let p = store.project_save(None, "Acme", "", "", now).unwrap();
        let save =
            |kind: &str, title: &str, url: Option<&str>, note: &str, options: serde_json::Value| {
                store
                    .resource_save(
                        &p.id,
                        &ResourceInput {
                            kind: kind.into(),
                            title: title.into(),
                            url: url.map(Into::into),
                            note: note.into(),
                            options: Some(options),
                            ..Default::default()
                        },
                        "user",
                        now,
                    )
                    .unwrap()
            };
        save(
            "note",
            "Refund policy",
            None,
            "Refunds within 30 days, no questions asked.",
            serde_json::json!({}),
        );
        save(
            "file",
            "Onboarding.md",
            Some("sha256:md"),
            "",
            serde_json::json!({"name": "Onboarding.md"}),
        );
        let mail = save(
            "email",
            "Billing",
            Some("label:billing"),
            "",
            serde_json::json!({}),
        );
        for t in store.knowledge_due(now, 10).unwrap() {
            index_one(&store, &gw, &NoSources, &t).await.unwrap();
        }
        let rs = store.project_resources(&p.id, "coordinator").unwrap();
        let by = |id: &str| rs.iter().find(|r| r.id == id).unwrap();
        assert!(
            rs.iter()
                .filter(|r| r.kind != "email")
                .all(|r| r.status == "indexed" && r.chunks == 1)
        );
        assert_eq!(by(&mail.id).status, "needs_sign_in");
        let hits = store
            .chunks_search("rollback fridays", None, &Reader::default(), 5)
            .unwrap();
        assert_eq!(hits[0].resource, "Onboarding.md");
        assert!(hits[0].text.starts_with("# Onboarding"));
        // Nothing due again until it changes.
        assert!(store.knowledge_due(now, 10).unwrap().is_empty());
    }
}
