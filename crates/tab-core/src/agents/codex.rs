//! Codex: its chats run over ACP through codex-acp (see `agents::acp`).
//! What stays here is strict mode's part, which codex-acp has no switch for:
//! a Codex home folder without the person's own setup.

use std::path::{Path, PathBuf};

/// What in Codex's home folder is the person's setup rather than their
/// sign-in and history: settings (MCP servers, hooks, profiles, default
/// instructions), global instructions, execpolicy rules, plugins, skills,
/// prompts and memories. A strict chat's Codex sees none of it.
fn personal(name: &str) -> bool {
    matches!(
        name,
        "config.toml"
            | "config.json"
            | "AGENTS.md"
            | "AGENTS.override.md"
            | "rules"
            | "plugins"
            | "skills"
            | "prompts"
            | "memories"
            | "hooks"
            | "hooks.json"
    ) || name.starts_with("memories_")
        // Profiles: `<name>.config.toml`.
        || name.ends_with(".config.toml")
}

/// The person's Codex home: `CODEX_HOME`, else `~/.codex`.
fn codex_home() -> Option<PathBuf> {
    std::env::var_os("CODEX_HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .or_else(|| crate::sandbox::home().map(|h| h.join(".codex")))
}

/// The strict chats' Codex home, `tab-strict` inside the person's: links to
/// everything there but their setup, so the sign-in (and its refreshes),
/// conversations and caches are shared, and `--ignore-user-config` has its
/// equivalent. Rebuilt before each turn, so what an agent leaves in it
/// doesn't last.
pub(super) fn strict_home() -> anyhow::Result<PathBuf> {
    let real = codex_home().ok_or_else(|| anyhow::anyhow!("no home folder for Codex"))?;
    std::fs::create_dir_all(&real)?;
    let home = real.join("tab-strict");
    build(&real, &home)?;
    Ok(home)
}

fn build(real: &Path, home: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(home)?;
    // Start over: links are made again, and any of the person's kind of
    // setup written here (an agent's own config.toml) goes. Other files are
    // the home's own (folders on Windows, where they can't be links).
    for e in std::fs::read_dir(home)?.flatten() {
        let p = e.path();
        let link = std::fs::symlink_metadata(&p).is_ok_and(|m| m.file_type().is_symlink());
        if !link && !personal(&e.file_name().to_string_lossy()) {
            continue;
        }
        let _ = if link || p.is_file() {
            std::fs::remove_file(&p)
        } else {
            std::fs::remove_dir_all(&p)
        };
    }
    for e in std::fs::read_dir(real)?.flatten() {
        let name = e.file_name();
        let n = name.to_string_lossy();
        if personal(&n) || n == "tab-strict" {
            continue;
        }
        let (from, to) = (e.path(), home.join(&name));
        if std::fs::symlink_metadata(&to).is_ok() {
            continue;
        }
        if let Err(err) = link(&from, &to) {
            tracing::debug!(error = %err, path = %from.display(), "couldn't link into Codex's strict home");
        }
    }
    Ok(())
}

#[cfg(unix)]
fn link(from: &Path, to: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(from, to)
}

/// Symlinks need Developer Mode on Windows: files fall back to hard links
/// (the sign-in is rewritten in place, which a hard link follows), and
/// folders stay the strict home's own.
#[cfg(windows)]
fn link(from: &Path, to: &Path) -> std::io::Result<()> {
    if from.is_dir() {
        std::os::windows::fs::symlink_dir(from, to)
    } else {
        std::os::windows::fs::symlink_file(from, to).or_else(|_| std::fs::hard_link(from, to))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(unix)]
    fn the_strict_home_has_the_sign_in_and_history_but_not_the_setup() {
        let root = std::env::temp_dir().join(format!("tab-codex-home-{}", uuid::Uuid::new_v4()));
        let real = root.join(".codex");
        std::fs::create_dir_all(real.join("sessions/2026")).unwrap();
        std::fs::create_dir_all(real.join("rules")).unwrap();
        for f in [
            "auth.json",
            "config.toml",
            "AGENTS.md",
            "work.config.toml",
            "memories_1.sqlite",
            "history.jsonl",
        ] {
            std::fs::write(real.join(f), f).unwrap();
        }
        let home = real.join("tab-strict");
        build(&real, &home).unwrap();
        assert_eq!(
            std::fs::read_to_string(home.join("auth.json")).unwrap(),
            "auth.json"
        );
        assert!(home.join("sessions/2026").is_dir());
        assert!(home.join("history.jsonl").exists());
        for gone in [
            "config.toml",
            "AGENTS.md",
            "rules",
            "work.config.toml",
            "memories_1.sqlite",
            "tab-strict",
        ] {
            assert!(!home.join(gone).exists(), "{gone}");
        }
        // Something an agent leaves there is gone by the next turn.
        std::fs::write(home.join("config.toml"), "[mcp_servers.x]").unwrap();
        build(&real, &home).unwrap();
        assert!(!home.join("config.toml").exists());
        // The sign-in's refreshes reach the person's own.
        std::fs::write(home.join("auth.json"), "refreshed").unwrap();
        assert_eq!(
            std::fs::read_to_string(real.join("auth.json")).unwrap(),
            "refreshed"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
