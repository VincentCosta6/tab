//! Values for a slash command's arguments that only the chat's folder knows:
//! its git branches and its files, for the composer's `/` menu.

use std::path::Path;

/// Most values offered at once.
const LIMIT: usize = 50;

/// The local branches of the repo at `cwd`, most recently committed first.
pub fn branches(cwd: &Path) -> Vec<String> {
    let out = std::process::Command::new("git")
        .args([
            "for-each-ref",
            "--sort=-committerdate",
            "--format=%(refname:short)",
            "refs/heads",
        ])
        .current_dir(cwd)
        .output();
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .lines()
            .filter(|l| !l.is_empty())
            .take(LIMIT)
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

/// Files and folders under `cwd` that `typed` could go on to name: the
/// entries of the folder it's in, whose names start with its last part.
/// Folders end in `/`; hidden entries show once `.` is typed.
pub fn paths(cwd: &Path, typed: &str) -> Vec<String> {
    let (dir, stem) = match typed.rfind('/') {
        Some(i) => (&typed[..=i], &typed[i + 1..]),
        None => ("", typed),
    };
    // Only inside the chat's folder.
    if dir.starts_with('/') || dir.split('/').any(|p| p == "..") {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(cwd.join(dir)) else {
        return Vec::new();
    };
    let stem_lower = stem.to_lowercase();
    let mut found: Vec<(bool, String)> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if (name.starts_with('.') && !stem.starts_with('.')) || name == ".git" {
                return None;
            }
            if !name.to_lowercase().starts_with(&stem_lower) {
                return None;
            }
            let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
            Some((
                !is_dir,
                format!("{dir}{name}{}", if is_dir { "/" } else { "" }),
            ))
        })
        .collect();
    // Folders first, then by name.
    found.sort();
    found.into_iter().take(LIMIT).map(|(_, p)| p).collect()
}

/// Most files listed from one folder for `@` mentions.
const MAX_FILES: usize = 50_000;

/// Folders skipped when listing a folder that isn't a git repo.
const SKIP: &[&str] = &[
    "node_modules",
    "target",
    ".git",
    "dist",
    "build",
    "__pycache__",
    ".venv",
];

/// Files under `cwd` that git doesn't ignore (or, outside a repo, all but
/// hidden ones and build output), as paths relative to it, with the
/// folders they're in. Kept for a few seconds, as it's asked for on every
/// key typed.
fn listing(cwd: &Path) -> std::sync::Arc<Vec<String>> {
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};
    type Cache = Mutex<Option<(std::path::PathBuf, Instant, Arc<Vec<String>>)>>;
    static CACHE: Cache = Mutex::new(None);
    if let Some((dir, at, list)) = CACHE.lock().unwrap().as_ref()
        && dir == cwd
        && at.elapsed() < Duration::from_secs(10)
    {
        return list.clone();
    }
    let out = std::process::Command::new("git")
        .args(["ls-files", "-co", "--exclude-standard", "-z"])
        .current_dir(cwd)
        .output();
    let mut files: Vec<String> = match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .split('\0')
            .filter(|f| !f.is_empty())
            .take(MAX_FILES)
            .map(str::to_string)
            .collect(),
        _ => walk(cwd),
    };
    let mut dirs = std::collections::BTreeSet::new();
    for f in &files {
        let mut rest = f.as_str();
        while let Some(i) = rest.rfind('/') {
            rest = &rest[..i];
            if !dirs.insert(format!("{rest}/")) {
                break;
            }
        }
    }
    files.extend(dirs);
    let list = Arc::new(files);
    *CACHE.lock().unwrap() = Some((cwd.to_path_buf(), Instant::now(), list.clone()));
    list
}

/// Every file under `cwd` but hidden ones and build output, breadth first.
fn walk(cwd: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut queue = std::collections::VecDeque::from([String::new()]);
    while let Some(dir) = queue.pop_front() {
        let Ok(entries) = std::fs::read_dir(cwd.join(&dir)) else {
            continue;
        };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || SKIP.contains(&name.as_str()) {
                continue;
            }
            let path = format!("{dir}{name}");
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                queue.push_back(format!("{path}/"));
            } else {
                out.push(path);
            }
            if out.len() >= MAX_FILES {
                return out;
            }
        }
    }
    out
}

/// How well `query` matches `path`, as its letters in order (higher is
/// better): letters that start a name or follow each other count more, and
/// so does matching in the last part of the path. None when it doesn't.
fn score(path: &str, query: &str) -> Option<i64> {
    let p: Vec<char> = path.to_lowercase().chars().collect();
    let end = p.len() - usize::from(p.last() == Some(&'/'));
    let base = p[..end]
        .iter()
        .rposition(|c| *c == '/')
        .map_or(0, |i| i + 1);
    let mut score = 0i64;
    let mut at = 0usize;
    let mut prev: Option<usize> = None;
    for q in query.to_lowercase().chars() {
        let i = (at..p.len()).find(|&i| p[i] == q)?;
        let start = i == 0 || matches!(p[i - 1], '/' | '_' | '-' | '.' | ' ');
        score += 1;
        if prev == Some(i.wrapping_sub(1)) {
            score += 5;
        }
        if start {
            score += 8;
        }
        if i >= base {
            score += 3;
        }
        prev = Some(i);
        at = i + 1;
    }
    // Shorter paths first among equals.
    Some(score * 100 - p.len() as i64)
}

/// Files and folders under `cwd` for an `@` mention, best match for
/// `query` first. With no query, the top of the folder.
pub fn mentions(cwd: &Path, query: &str) -> Vec<String> {
    if query.is_empty() {
        return paths(cwd, "");
    }
    // A path typed out goes on through its folder.
    if query.contains('/') && cwd.join(&query[..=query.rfind('/').unwrap()]).is_dir() {
        let found = paths(cwd, query);
        if !found.is_empty() {
            return found;
        }
    }
    let list = listing(cwd);
    let mut found: Vec<(i64, &String)> = list
        .iter()
        .filter_map(|f| score(f, query).map(|s| (s, f)))
        .collect();
    found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(b.1)));
    found
        .into_iter()
        .take(LIMIT)
        .map(|(_, f)| f.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mentions_find_files_by_letters_in_order() {
        let dir = std::env::temp_dir().join(format!("tab-mentions-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("src/modules/work")).unwrap();
        std::fs::create_dir_all(dir.join("node_modules/x")).unwrap();
        std::fs::write(dir.join("src/modules/work/Chat.tsx"), "").unwrap();
        std::fs::write(dir.join("src/chart.ts"), "").unwrap();
        std::fs::write(dir.join("node_modules/x/chat.js"), "").unwrap();
        std::fs::write(dir.join("README.md"), "").unwrap();
        let found = mentions(&dir, "chat");
        assert_eq!(
            found.first().map(String::as_str),
            Some("src/modules/work/Chat.tsx")
        );
        assert!(!found.iter().any(|f| f.starts_with("node_modules")));
        assert!(mentions(&dir, "smw").contains(&"src/modules/work/".to_string()));
        assert_eq!(mentions(&dir, "src/"), ["src/modules/", "src/chart.ts"]);
        assert!(mentions(&dir, "zzz").is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn paths_complete_inside_the_folder_only() {
        let dir = std::env::temp_dir().join(format!("tab-paths-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("src/lib")).unwrap();
        std::fs::write(dir.join("src/main.rs"), "").unwrap();
        std::fs::write(dir.join("README.md"), "").unwrap();
        std::fs::write(dir.join(".env"), "").unwrap();
        assert_eq!(paths(&dir, ""), ["src/", "README.md"]);
        assert_eq!(paths(&dir, "s"), ["src/"]);
        assert_eq!(paths(&dir, "src/"), ["src/lib/", "src/main.rs"]);
        assert_eq!(paths(&dir, "src/M"), ["src/main.rs"]);
        assert_eq!(paths(&dir, "."), [".env"]);
        assert!(paths(&dir, "../").is_empty());
        assert!(paths(&dir, "/etc/").is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }
}
