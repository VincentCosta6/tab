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

#[cfg(test)]
mod tests {
    use super::*;

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
