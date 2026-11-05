//! Local embeddings for search ("Search by meaning", opt-in on the tab
//! connection).
//!
//! The models are Model2Vec static embeddings: a lookup table, no neural
//! network at query time, so they embed thousands of items a second on a
//! laptop CPU with no GPU or API key. The chosen model is downloaded once
//! from Hugging Face into the app data dir and quantized to int8 when
//! loaded (a quarter of the memory, nearly the same results). While it's
//! off, or still downloading, search is keyword-only and everything else
//! works as before. Turning it off deletes the model and the index.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use model2vec_rs::model::StaticModel;
use tokio::sync::watch;

use crate::config::TabConfig;
use crate::store::Store;

/// Smaller download, weaker matching: 30 MB, 256 dimensions.
pub const SMALL_MODEL: &str = "minishlab/potion-base-8M";
/// Tuned for retrieval: 130 MB, 512 dimensions.
pub const LARGE_MODEL: &str = "minishlab/potion-retrieval-32M";
const FILES: [&str; 3] = ["config.json", "tokenizer.json", "model.safetensors"];
const BATCH: usize = 128;

/// What the index is doing, for the Connections page and for agents.
#[derive(Debug, Clone, Default, serde::Serialize, PartialEq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum IndexStatus {
    #[default]
    Off,
    Downloading {
        model: String,
        received: u64,
        total: Option<u64>,
    },
    Loading {
        model: String,
    },
    Indexing {
        model: String,
        done: i64,
        total: i64,
    },
    Ready {
        model: String,
        dim: usize,
        docs: i64,
        indexed_at: Option<chrono::DateTime<Utc>>,
    },
    Error {
        model: String,
        message: String,
        retry_at: chrono::DateTime<Utc>,
    },
}

impl IndexStatus {
    /// A short line for agents' search results.
    pub fn summary(&self) -> String {
        match self {
            Self::Off => "keyword only (search by meaning is off; the person can turn it on in Connections, under tab)".into(),
            Self::Downloading { .. } | Self::Loading { .. } => "keyword only (the meaning model is still downloading)".into(),
            Self::Indexing { done, total, .. } => format!(
                "keyword + meaning, partial: {done} of {total} items indexed so far"
            ),
            Self::Ready { .. } => "keyword + meaning".into(),
            Self::Error { message, .. } => format!("keyword only (meaning model failed: {message})"),
        }
    }
}

/// The model the settings ask for, or `None` when search by meaning is off.
pub fn choice(cfg: &TabConfig) -> Option<&'static str> {
    let s = &cfg.connectors.get("tab")?.settings;
    let on = |k: &str| s.get(k).and_then(|v| v.as_bool()).unwrap_or(false);
    on("semantic_search").then(|| {
        if on("semantic_model_large") {
            LARGE_MODEL
        } else {
            SMALL_MODEL
        }
    })
}

/// Turns text into vectors. A trait so tests can plug in a toy.
pub trait Embed: Send + Sync {
    fn name(&self) -> &str;
    fn dim(&self) -> usize;
    fn encode(&self, texts: &[String]) -> Vec<Vec<f32>>;
}

pub struct Model2Vec {
    name: String,
    model: StaticModel,
}

impl Embed for Model2Vec {
    fn name(&self) -> &str {
        &self.name
    }
    fn dim(&self) -> usize {
        self.model.dim()
    }
    fn encode(&self, texts: &[String]) -> Vec<Vec<f32>> {
        self.model.encode(texts)
    }
}

fn models_root(data_dir: &Path) -> PathBuf {
    data_dir.join("models")
}

fn model_dir(data_dir: &Path, repo: &str) -> PathBuf {
    models_root(data_dir).join(repo.replace('/', "--"))
}

/// Fetches the model files that are missing. Each file is written to a
/// temporary name first, so an interrupted download is retried, not loaded.
async fn download(store: &Store, dir: &Path, repo: &str) -> anyhow::Result<()> {
    tokio::fs::create_dir_all(dir).await?;
    let http = reqwest::Client::builder()
        .user_agent("tab-desktop")
        .connect_timeout(Duration::from_secs(20))
        .build()?;
    for f in FILES {
        let path = dir.join(f);
        if tokio::fs::try_exists(&path).await? {
            continue;
        }
        let url = format!("https://huggingface.co/{repo}/resolve/main/{f}");
        tracing::info!(%url, "downloading embedding model file");
        let mut res = http.get(&url).send().await?.error_for_status()?;
        let total = res.content_length();
        let mut bytes = Vec::with_capacity(total.unwrap_or(0) as usize);
        while let Some(chunk) = res.chunk().await? {
            bytes.extend_from_slice(&chunk);
            store.set_index_status(IndexStatus::Downloading {
                model: repo.into(),
                received: bytes.len() as u64,
                total,
            });
        }
        let tmp = dir.join(format!("{f}.part"));
        tokio::fs::write(&tmp, &bytes).await?;
        tokio::fs::rename(&tmp, &path).await?;
    }
    Ok(())
}

async fn load(
    store: &Store,
    data_dir: &Path,
    repo: &'static str,
) -> anyhow::Result<Arc<dyn Embed>> {
    let dir = model_dir(data_dir, repo);
    download(store, &dir, repo).await?;
    store.set_index_status(IndexStatus::Loading { model: repo.into() });
    let model = tokio::task::spawn_blocking(move || {
        StaticModel::from_pretrained(&dir, None, Some(true), None).map(StaticModel::quantize_int8)
    })
    .await??;
    Ok(Arc::new(Model2Vec {
        name: repo.into(),
        model,
    }))
}

/// Deletes downloaded models other than `keep`.
async fn remove_models(data_dir: &Path, keep: Option<&str>) {
    let root = models_root(data_dir);
    let Ok(mut entries) = tokio::fs::read_dir(&root).await else {
        return;
    };
    let keep = keep.map(|k| k.replace('/', "--"));
    while let Ok(Some(e)) = entries.next_entry().await {
        if keep.as_deref() != e.file_name().to_str() {
            match tokio::fs::remove_dir_all(e.path()).await {
                Ok(()) => tracing::info!(path = %e.path().display(), "removed embedding model"),
                Err(err) => tracing::warn!(error = %err, "couldn't remove embedding model"),
            }
        }
    }
}

/// Embeds whatever is new or changed, until nothing is. Returns how many.
pub async fn index_pending(store: &Arc<Store>, model: &Arc<dyn Embed>) -> anyhow::Result<usize> {
    let mut total = 0;
    let ready = |store: &Store| -> anyhow::Result<IndexStatus> {
        let (docs, _) = store.vectors_status()?;
        Ok(IndexStatus::Ready {
            model: model.name().into(),
            dim: model.dim(),
            docs,
            indexed_at: store.vectors_last_indexed()?,
        })
    };
    loop {
        let docs = store.vectors_pending(BATCH)?;
        if docs.is_empty() {
            store.set_index_status(ready(store)?);
            return Ok(total);
        }
        let (done, waiting) = store.vectors_status()?;
        store.set_index_status(IndexStatus::Indexing {
            model: model.name().into(),
            done,
            total: done + waiting,
        });
        let model = model.clone();
        let texts: Vec<String> = docs.iter().map(|d| d.text.clone()).collect();
        let vecs = tokio::task::spawn_blocking(move || model.encode(&texts)).await?;
        let pairs: Vec<_> = docs.into_iter().map(|d| d.key).zip(vecs).collect();
        store.vectors_store(&pairs, Utc::now())?;
        total += pairs.len();
        // Leave the database to the UI between batches.
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Follows the setting: when on, loads (downloading if needed) the chosen
/// model and keeps the index current; when off, removes model and index.
pub fn spawn_indexer(
    store: Arc<Store>,
    data_dir: PathBuf,
    mut want: watch::Receiver<Option<&'static str>>,
) {
    tokio::spawn(async move {
        loop {
            let repo = *want.borrow_and_update();
            let Some(repo) = repo else {
                store.set_index_status(IndexStatus::Off);
                store.clear_embedder();
                if let Err(e) = store.vectors_clear() {
                    tracing::warn!(error = %e, "couldn't clear the vector index");
                }
                remove_models(&data_dir, None).await;
                if want.changed().await.is_err() {
                    return;
                }
                continue;
            };
            remove_models(&data_dir, Some(repo)).await;
            // Load, retrying with backoff while offline; a settings change
            // starts over.
            let mut wait = Duration::from_secs(30);
            let model = loop {
                match load(&store, &data_dir, repo).await {
                    Ok(m) => break Some(m),
                    Err(e) => {
                        store.set_index_status(IndexStatus::Error {
                            model: repo.into(),
                            message: e.to_string(),
                            retry_at: Utc::now()
                                + chrono::Duration::from_std(wait).unwrap_or_default(),
                        });
                        tracing::warn!(error = %e, retry_in = ?wait, "embedding model unavailable; search stays keyword-only");
                        tokio::select! {
                            _ = tokio::time::sleep(wait) => {}
                            r = want.changed() => { if r.is_err() { return; } break None; }
                        }
                        wait = (wait * 2).min(Duration::from_secs(1800));
                    }
                }
            };
            let Some(model) = model else { continue };
            if let Err(e) = store.vectors_prepare(model.name(), model.dim()) {
                tracing::error!(error = %e, "couldn't create the vector index");
                if want.changed().await.is_err() {
                    return;
                }
                continue;
            }
            store.set_embedder(model.clone());
            tracing::info!(model = model.name(), dim = model.dim(), "embeddings ready");
            loop {
                match index_pending(&store, &model).await {
                    Ok(0) => {}
                    Ok(n) => tracing::debug!(n, "embedded"),
                    Err(e) => tracing::warn!(error = %e, "embedding failed"),
                }
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(15)) => {}
                    r = want.changed() => { if r.is_err() { return; } break; }
                }
            }
        }
    });
}
