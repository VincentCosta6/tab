//! What tab keeps on disk outside its database, and clearing out what's no
//! longer needed.
//!
//! - `attachments/`: files the person attached to chats.
//! - `sessions/<id>/`: each chat's system prompt and MCP config, rewritten
//!   at the start of every turn.
//! - `workspaces/`: local checkouts for PR reviews (see [`crate::workspace`]).
//!
//! Attachments and session folders are swept here; checkouts are swept by
//! the app, which can ask GitHub whether a PR has closed.

use std::collections::HashSet;
use std::path::{Component, Path};
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::store::Store;

/// Attachments from chats idle this long are removed, as are session folders.
pub const IDLE_CHAT: Duration = Duration::from_secs(30 * 86_400);
/// An attachment no message refers to (attached, then never sent) is kept
/// this long, so a message being written doesn't lose its files.
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

/// Removes attachments no chat active in the last [`IDLE_CHAT`] refers to,
/// once they're older than [`UNSENT_GRACE`].
pub fn sweep_attachments(
    store: &Store,
    dir: &Path,
    now: DateTime<Utc>,
    idle: Duration,
) -> anyhow::Result<Swept> {
    let since = now - chrono::Duration::from_std(idle)?;
    let keep: HashSet<String> = store
        .attachments_in_use(since)?
        .iter()
        .filter_map(|p| top_entry(dir, p))
        .collect();
    let mut swept = Swept::default();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(swept);
    };
    let clock = SystemTime::from(now);
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if keep.contains(&name) || age(&e.path(), clock) < UNSENT_GRACE {
            continue;
        }
        if let Some(bytes) = remove(&e.path()) {
            swept.add(Swept { removed: 1, bytes });
        }
    }
    Ok(swept)
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
        store
            .message_add(&s.id, "user", "text", "hi", &meta, now)
            .unwrap();

        // Nothing is old enough yet.
        let swept = sweep_attachments(&store, &dir, now, IDLE_CHAT).unwrap();
        assert_eq!(swept, Swept::default());

        // A day on, the file that was never sent goes.
        let soon = now + chrono::Duration::days(2);
        let swept = sweep_attachments(&store, &dir, soon, IDLE_CHAT).unwrap();
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

        // Once the chat has been idle long enough, its files go too.
        let later = now + chrono::Duration::days(31);
        let swept = sweep_attachments(&store, &dir, later, IDLE_CHAT).unwrap();
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
