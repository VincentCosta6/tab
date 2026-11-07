//! What tab keeps on disk outside its database, and clearing out what's no
//! longer needed.
//!
//! - `attachments/`: files the person attached to chats.
//! - `sessions/<id>/`: each chat's system prompt and MCP config, rewritten
//!   at the start of every turn.
//! - `workspaces/`: local checkouts for PR reviews (see [`crate::workspace`]).
//!
//! Two rules, both set in Settings → Storage ([`crate::config::StorageConfig`]):
//! anything unused for `keep_days` goes, and past `max_gb` the least
//! recently used attachments and checkouts go until it fits ([`evict`]).
//!
//! Attachments and session folders are swept here; checkouts are swept by
//! the app, which can ask GitHub whether a PR has closed.

use std::collections::HashMap;
use std::path::PathBuf;
use std::path::{Component, Path};
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::store::Store;

/// Nothing used this recently is evicted for space, so a size limit never
/// pulls a file or checkout out from under you.
pub const EVICT_GRACE: Duration = Duration::from_secs(3600);
/// A chat folder newer than this is never removed, even if its chat looks
/// idle, so one being set up for a first turn is left alone.
pub const UNSENT_GRACE: Duration = Duration::from_secs(86_400);

/// Size of one folder.
#[derive(Debug, Clone, Copy, Default, Serialize, PartialEq)]
pub struct Size {
    pub bytes: u64,
    pub files: u64,
}

/// Adds up a folder's files. Symlinks aren't followed. Missing is zero.
pub fn dir_size(path: &Path) -> Size {
    let mut size = Size::default();
    let mut stack = vec![path.to_path_buf()];
    while let Some(p) = stack.pop() {
        let Ok(meta) = std::fs::symlink_metadata(&p) else {
            continue;
        };
        if meta.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&p) {
                stack.extend(entries.flatten().map(|e| e.path()));
            }
        } else {
            size.bytes += meta.len();
            size.files += 1;
        }
    }
    size
}

/// What a sweep removed.
#[derive(Debug, Clone, Copy, Default, Serialize, PartialEq)]
pub struct Swept {
    pub removed: u64,
    pub bytes: u64,
}

impl Swept {
    pub fn add(&mut self, other: Swept) {
        self.removed += other.removed;
        self.bytes += other.bytes;
    }
}

fn age(path: &Path, now: SystemTime) -> Duration {
    std::fs::symlink_metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| now.duration_since(t).ok())
        .unwrap_or_default()
}

fn remove(path: &Path) -> Option<u64> {
    let size = dir_size(path).bytes;
    let res = if std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir()) {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    match res {
        Ok(()) => Some(size),
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "couldn't remove");
            None
        }
    }
}

/// The top-level entry under `dir` that `path` lives in: attachments are
/// saved as `<uuid>.png` or `<uuid>/<name>`.
fn top_entry(dir: &Path, path: &str) -> Option<String> {
    match Path::new(path)
        .strip_prefix(dir)
        .ok()?
        .components()
        .next()?
    {
        Component::Normal(s) => s.to_str().map(String::from),
        _ => None,
    }
}

/// Each top-level entry in the attachments folder with when it was last
/// used: the later of when it was saved and when a chat using it was last
/// active.
pub fn attachments(
    store: &Store,
    dir: &Path,
    now: DateTime<Utc>,
) -> anyhow::Result<Vec<Candidate<PathBuf>>> {
    let mut used: HashMap<String, DateTime<Utc>> = HashMap::new();
    for (p, t) in store.attachments_last_used(now)? {
        if let Some(top) = top_entry(dir, &p) {
            let e = used.entry(top).or_insert(t);
            *e = (*e).max(t);
        }
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(Vec::new());
    };
    Ok(entries
        .flatten()
        .map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let saved: DateTime<Utc> = std::fs::symlink_metadata(e.path())
                .and_then(|m| m.modified())
                .map(Into::into)
                .unwrap_or(now);
            let last_used = used.get(&name).map_or(saved, |t| (*t).max(saved));
            Candidate {
                bytes: dir_size(&e.path()).bytes,
                key: e.path(),
                last_used,
            }
        })
        .collect())
}

/// Removes attachments not used for `keep`: in no chat active since then,
/// and saved before then.
pub fn sweep_attachments(
    store: &Store,
    dir: &Path,
    now: DateTime<Utc>,
    keep: Duration,
) -> anyhow::Result<Swept> {
    let since = now - chrono::Duration::from_std(keep)?;
    let mut swept = Swept::default();
    for c in attachments(store, dir, now)? {
        if c.last_used < since
            && let Some(bytes) = remove(&c.key)
        {
            swept.add(Swept { removed: 1, bytes });
        }
    }
    Ok(swept)
}

/// Something that can be removed to make room.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate<K> {
    pub key: K,
    pub last_used: DateTime<Utc>,
    /// What removing it frees.
    pub bytes: u64,
}

/// Picks what to remove, least recently used first, to bring `total` down
/// to `limit`. Nothing used since `protect_since` is picked, so this may
/// fall short.
pub fn evict<K>(
    mut candidates: Vec<Candidate<K>>,
    total: u64,
    limit: u64,
    protect_since: DateTime<Utc>,
) -> Vec<Candidate<K>> {
    candidates.retain(|c| c.last_used < protect_since);
    candidates.sort_by_key(|c| c.last_used);
    let mut over = total.saturating_sub(limit);
    let mut out = Vec::new();
    for c in candidates {
        if over == 0 {
            break;
        }
        over = over.saturating_sub(c.bytes);
        out.push(c);
    }
    out
}

/// Removes a file or folder, returning what it freed.
pub fn remove_path(path: &Path) -> Swept {
    remove(path).map_or_else(Swept::default, |bytes| Swept { removed: 1, bytes })
}

/// Removes the per-chat folders of chats idle for longer than `idle`. They
/// are rewritten at the start of a chat's next turn, so this loses nothing.
pub fn sweep_sessions(
    store: &Store,
    dir: &Path,
    now: DateTime<Utc>,
    idle: Duration,
) -> anyhow::Result<Swept> {
    let since = now - chrono::Duration::from_std(idle)?;
    let active = store.sessions_active(since)?;
    let mut swept = Swept::default();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(swept);
    };
    let clock = SystemTime::from(now);
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if active.contains(&name) || age(&e.path(), clock) < UNSENT_GRACE {
            continue;
        }
        if let Some(bytes) = remove(&e.path()) {
            swept.add(Swept { removed: 1, bytes });
        }
    }
    Ok(swept)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::AgentKind;

    const WEEK: Duration = Duration::from_secs(7 * 86_400);

    #[test]
    fn sweeps_only_unused_old_attachments() {
        let root = std::env::temp_dir().join(format!("tab-storage-{}", uuid::Uuid::new_v4()));
        let dir = root.join("attachments");
        std::fs::create_dir_all(dir.join("kept")).unwrap();
        std::fs::create_dir_all(dir.join("unsent")).unwrap();
        std::fs::write(dir.join("kept/a.txt"), "a").unwrap();
        std::fs::write(dir.join("unsent/b.txt"), "bb").unwrap();
        std::fs::write(dir.join("kept.png"), "c").unwrap();

        let store = Store::in_memory().unwrap();
        let now = Utc::now();
        let s = store
            .session_create(None, AgentKind::ClaudeCode, "t", "/", now)
            .unwrap();
        let used = [
            dir.join("kept/a.txt").display().to_string(),
            dir.join("kept.png").display().to_string(),
        ];
        let meta = serde_json::json!({ "attachments": used });
        let later = now + chrono::Duration::days(5);
        store
            .message_add(&s.id, "user", "text", "hi", &meta, later)
            .unwrap();

        // Nothing is a week old yet.
        let swept = sweep_attachments(&store, &dir, now, WEEK).unwrap();
        assert_eq!(swept, Swept::default());

        // Eight days on, the file that was never sent goes; the ones sent
        // on day five stay.
        let soon = now + chrono::Duration::days(8);
        let swept = sweep_attachments(&store, &dir, soon, WEEK).unwrap();
        assert_eq!(
            swept,
            Swept {
                removed: 1,
                bytes: 2
            }
        );
        assert!(dir.join("kept/a.txt").exists());
        assert!(dir.join("kept.png").exists());
        assert!(!dir.join("unsent").exists());

        // A week after the chat was last used, its files go too.
        let gone = now + chrono::Duration::days(13);
        let swept = sweep_attachments(&store, &dir, gone, WEEK).unwrap();
        assert_eq!(
            swept,
            Swept {
                removed: 2,
                bytes: 2
            }
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn evicts_least_recently_used_until_it_fits() {
        let now = Utc::now();
        let c = |key: &'static str, hours_ago: i64, bytes: u64| Candidate {
            key,
            last_used: now - chrono::Duration::hours(hours_ago),
            bytes,
        };
        let all = vec![
            c("new", 0, 500),
            c("oldest", 100, 10),
            c("middle", 50, 30),
            c("old", 80, 20),
        ];
        let keys = |v: Vec<Candidate<&'static str>>| v.into_iter().map(|c| c.key).collect::<Vec<_>>();
        let grace = now - chrono::Duration::hours(1);
        // 25 over: the two oldest free 30.
        assert_eq!(keys(evict(all.clone(), 125, 100, grace)), ["oldest", "old"]);
        assert!(evict(all.clone(), 100, 100, grace).is_empty());
        // Way over, but the newest is in use, so it stays.
        assert_eq!(
            keys(evict(all, 10_000, 0, grace)),
            ["oldest", "old", "middle"]
        );
    }

    #[test]
    fn sizes_a_tree() {
        let root = std::env::temp_dir().join(format!("tab-size-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        std::fs::write(root.join("a/b/x"), "123").unwrap();
        std::fs::write(root.join("y"), "45").unwrap();
        assert_eq!(dir_size(&root), Size { bytes: 5, files: 2 });
        assert_eq!(dir_size(&root.join("missing")), Size::default());
        let _ = std::fs::remove_dir_all(&root);
    }
}
