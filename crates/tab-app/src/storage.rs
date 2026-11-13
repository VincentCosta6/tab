//! What tab keeps on disk (chat attachments, chat folders, review
//! checkouts), measuring it and clearing it out by Settings → Storage.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::Serialize;
use tab_core::agents::AgentHub;
use tab_core::gateway::{Call, Gateway};
use tab_core::storage;
use tab_core::workspace::Workspaces;

use crate::Backend;

/// A closed or merged PR's checkout stays this long after it was last opened,
/// so one being looked at right as it merges isn't pulled out from under you.
const CHECKOUT_GRACE: chrono::Duration = chrono::Duration::days(1);
/// How often the cleanup sweep runs.
pub const SWEEP_EVERY: std::time::Duration = std::time::Duration::from_secs(6 * 3600);

#[derive(Debug, Clone, Serialize)]
pub struct CheckoutUsage {
    pub repo: String,
    pub number: u64,
    pub last_used: DateTime<Utc>,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct StorageUsage {
    pub attachments: storage::Size,
    pub sessions: storage::Size,
    pub workspaces: storage::Size,
    pub checkouts: Vec<CheckoutUsage>,
}

/// Something the size limit can remove.
enum Evictable {
    Attachment(PathBuf),
    Checkout(String, u64),
}

impl Backend {
    /// How much space tab's files take.
    pub async fn storage_usage(&self) -> Result<StorageUsage, String> {
        let hub = self.agents.clone();
        let ws = self.workspaces.clone();
        let data = self.paths.data_dir.clone();
        tokio::task::spawn_blocking(move || StorageUsage {
            checkouts: ws
                .checkouts()
                .into_iter()
                .map(|c| CheckoutUsage {
                    bytes: ws
                        .worktree(&c.repo, c.number)
                        .map(|d| storage::dir_size(&d).bytes)
                        .unwrap_or(0),
                    repo: c.repo,
                    number: c.number,
                    last_used: c.last_used,
                })
                .collect(),
            attachments: storage::dir_size(&hub.attachments_dir()),
            sessions: storage::dir_size(&hub.sessions_dir()),
            workspaces: storage::dir_size(&data.join("workspaces")),
        })
        .await
        .map_err(crate::err)
    }

    /// Clears out attachments, chat folders and review checkouts by the
    /// storage rules: unused for `keep_days`, then past `max_gb`.
    pub async fn storage_clean(&self) -> storage::Swept {
        let cfg = self.config.lock().await.storage.clone();
        let keep = cfg.keep();
        let hub = self.agents.clone();
        let h = hub.clone();
        let mut swept = tokio::task::spawn_blocking(move || h.sweep_files(Utc::now(), keep))
            .await
            .map_err(anyhow::Error::from)
            .and_then(|r| r)
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "sweeping chat files failed");
                storage::Swept::default()
            });
        let ws = self.workspaces.clone();
        swept.add(sweep_checkouts(&ws, &self.gateway, keep).await);
        // Shared dependency installs no checkout links to any more.
        let live: Vec<PathBuf> = ws
            .checkouts()
            .iter()
            .filter_map(|c| ws.worktree(&c.repo, c.number).ok())
            .collect();
        swept.add(ws.deps().sweep(&live, keep).await);
        if let Some(limit) = cfg.max_bytes() {
            swept.add(enforce_limit(hub, ws, self.paths.data_dir.clone(), limit).await);
        }
        swept
    }

    /// Removes one review checkout, or every one when `which` is `None`.
    /// They come back the next time their PR is opened.
    pub async fn storage_remove_checkouts(&self, which: Option<(String, u64)>) -> storage::Swept {
        let mut swept = storage::Swept::default();
        for c in self.workspaces.checkouts() {
            let pick = match &which {
                Some((r, n)) => *r == c.repo && *n == c.number,
                None => true,
            };
            if pick {
                swept.add(remove_checkout(&self.workspaces, &c.repo, c.number).await);
            }
        }
        swept
    }
}

/// Runs the sweep shortly after launch, then every few hours.
pub fn spawn_sweeps(backend: std::sync::Weak<Backend>) {
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(120)).await;
        loop {
            let Some(b) = backend.upgrade() else { return };
            let s = b.storage_clean().await;
            drop(b);
            if s.removed > 0 {
                tracing::info!(removed = s.removed, bytes = s.bytes, "storage sweep");
            }
            tokio::time::sleep(SWEEP_EVERY).await;
        }
    });
}

/// Total size of everything the storage settings cover.
fn storage_total(hub: &AgentHub, data_dir: &Path) -> u64 {
    [
        hub.attachments_dir(),
        hub.sessions_dir(),
        data_dir.join("workspaces"),
    ]
    .iter()
    .map(|d| storage::dir_size(d).bytes)
    .sum()
}

/// Past the size limit, removes the least recently used attachments and
/// review checkouts until it fits.
async fn enforce_limit(
    hub: Arc<AgentHub>,
    ws: Arc<Workspaces>,
    data_dir: PathBuf,
    limit: u64,
) -> storage::Swept {
    let mut swept = storage::Swept::default();
    // Removing a repository's last checkout also frees its clone, which the
    // plan can't see, so re-measure and go again if still over.
    for _ in 0..3 {
        let (h, w, d) = (hub.clone(), ws.clone(), data_dir.clone());
        let plan = tokio::task::spawn_blocking(move || {
            let now = Utc::now();
            let total = storage_total(&h, &d);
            if total <= limit {
                return Vec::new();
            }
            let mut candidates: Vec<storage::Candidate<Evictable>> = h
                .attachments(now)
                .unwrap_or_default()
                .into_iter()
                .map(|c| storage::Candidate {
                    key: Evictable::Attachment(c.key),
                    last_used: c.last_used,
                    bytes: c.bytes,
                })
                .collect();
            candidates.extend(w.checkouts().into_iter().map(|c| {
                storage::Candidate {
                    bytes: w
                        .worktree(&c.repo, c.number)
                        .map(|p| storage::dir_size(&p).bytes)
                        .unwrap_or(0),
                    key: Evictable::Checkout(c.repo, c.number),
                    last_used: c.last_used,
                }
            }));
            let grace = chrono::Duration::from_std(storage::EVICT_GRACE).unwrap_or_default();
            storage::evict(candidates, total, limit, now - grace)
        })
        .await
        .unwrap_or_default();
        if plan.is_empty() {
            break;
        }
        for c in plan {
            swept.add(match c.key {
                Evictable::Attachment(p) => storage::remove_path(&p),
                Evictable::Checkout(repo, n) => remove_checkout(&ws, &repo, n).await,
            });
        }
    }
    swept
}

async fn remove_checkout(ws: &Workspaces, repo: &str, number: u64) -> storage::Swept {
    let Ok(dir) = ws.worktree(repo, number) else {
        return storage::Swept::default();
    };
    let bytes = storage::dir_size(&dir).bytes;
    match ws.remove(repo, number).await {
        Ok(()) => storage::Swept { removed: 1, bytes },
        Err(e) => {
            tracing::warn!(%repo, number, error = %e, "couldn't remove review checkout");
            storage::Swept::default()
        }
    }
}

/// Whether a PR has closed or merged. Errors (no GitHub, offline) say no.
async fn pr_closed(gateway: &Gateway, repo: &str, number: u64) -> bool {
    #[derive(serde::Deserialize)]
    struct Pr {
        state: String,
    }
    let url = format!("https://api.github.com/repos/{repo}/pulls/{number}");
    let call = Call::get("github", "github", "review checkout cleanup", url)
        .cache(std::time::Duration::from_secs(3600));
    let res = tab_core::ratelimit::background(gateway.send(call)).await;
    res.map_err(anyhow::Error::from)
        .and_then(|r| r.json::<Pr>())
        .map(|pr| pr.state != "open")
        .unwrap_or(false)
}

/// Removes review checkouts whose PR has closed, or that nobody has opened
/// for `keep`.
async fn sweep_checkouts(
    ws: &Workspaces,
    gateway: &Gateway,
    keep: std::time::Duration,
) -> storage::Swept {
    let now = Utc::now();
    let keep = chrono::Duration::from_std(keep).unwrap_or(chrono::Duration::days(7));
    let mut swept = storage::Swept::default();
    for c in ws.checkouts() {
        let idle = now - c.last_used;
        if idle > keep || (idle > CHECKOUT_GRACE && pr_closed(gateway, &c.repo, c.number).await) {
            swept.add(remove_checkout(ws, &c.repo, c.number).await);
        }
    }
    swept
}
