//! Local embeddings for search, built in the background.
//!
//! The model is Model2Vec's `potion-retrieval-32M`: a static embedding model
//! (a lookup table, no neural network at query time), so it embeds thousands
//! of items a second on a laptop CPU and needs no GPU or API key. It is
//! downloaded once from Hugging Face into the app data dir. Until it's
//! there, search is keyword-only and everything else works as before.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use model2vec_rs::model::StaticModel;

use crate::store::Store;

pub const MODEL_REPO: &str = "minishlab/potion-retrieval-32M";
const FILES: [&str; 3] = ["config.json", "tokenizer.json", "model.safetensors"];
const BATCH: usize = 128;

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

fn model_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("models").join(MODEL_REPO.replace('/', "--"))
}

/// Fetches the model files that are missing. Each file is written to a
/// temporary name first, so an interrupted download is retried, not loaded.
async fn download(dir: &Path) -> anyhow::Result<()> {
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
        let url = format!("https://huggingface.co/{MODEL_REPO}/resolve/main/{f}");
        tracing::info!(%url, "downloading embedding model file");
        let bytes = http
            .get(&url)
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?;
        let tmp = dir.join(format!("{f}.part"));
        tokio::fs::write(&tmp, &bytes).await?;
        tokio::fs::rename(&tmp, &path).await?;
    }
    Ok(())
}

async fn load(data_dir: &Path) -> anyhow::Result<Arc<dyn Embed>> {
    let dir = model_dir(data_dir);
    download(&dir).await?;
    let model = tokio::task::spawn_blocking(move || {
        StaticModel::from_pretrained(&dir, None, Some(true), None)
    })
    .await??;
    Ok(Arc::new(Model2Vec {
        name: MODEL_REPO.into(),
        model,
    }))
}

/// Embeds whatever is new or changed, until nothing is. Returns how many.
pub async fn index_pending(store: &Arc<Store>, model: &Arc<dyn Embed>) -> anyhow::Result<usize> {
    let mut total = 0;
    loop {
        let docs = store.vectors_pending(BATCH)?;
        if docs.is_empty() {
            return Ok(total);
        }
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

/// Loads (downloading if needed) the model, then keeps the index current.
/// Retries the download with backoff when offline.
pub fn spawn_indexer(store: Arc<Store>, data_dir: PathBuf) {
    tokio::spawn(async move {
        let mut wait = Duration::from_secs(30);
        let model = loop {
            match load(&data_dir).await {
                Ok(m) => break m,
                Err(e) => {
                    tracing::warn!(error = %e, retry_in = ?wait, "embedding model unavailable; search stays keyword-only");
                    tokio::time::sleep(wait).await;
                    wait = (wait * 2).min(Duration::from_secs(1800));
                }
            }
        };
        if let Err(e) = store.vectors_prepare(model.name(), model.dim()) {
            tracing::error!(error = %e, "couldn't create the vector index");
            return;
        }
        store.set_embedder(model.clone());
        tracing::info!(model = model.name(), dim = model.dim(), "embeddings ready");
        loop {
            match index_pending(&store, &model).await {
                Ok(0) => {}
                Ok(n) => tracing::debug!(n, "embedded"),
                Err(e) => tracing::warn!(error = %e, "embedding failed"),
            }
            tokio::time::sleep(Duration::from_secs(15)).await;
        }
    });
}
