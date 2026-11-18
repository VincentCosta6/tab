//! A project's own instructions in every harness. Each harness loads only
//! the instruction files it knows (Claude Code `CLAUDE.md`, Codex
//! `AGENTS.md`, Antigravity `GEMINI.md`, ...), so the same repo would give
//! one agent its rules and another none. tab finds the project's file and,
//! when the chat's harness wouldn't load it, adds it to the system prompt.

use std::path::Path;

use super::catalog;
use crate::model::AgentKind;

/// The files tab looks for, the first found in a folder being that
/// folder's instructions.
const FILES: &[&str] = &["AGENTS.md", "CLAUDE.md", "GEMINI.md"];

/// The most of a project's instructions tab carries, over all its folders.
const MAX: usize = 16_000;

/// One folder's instructions, as the harness's system prompt gets them.
#[derive(Debug, PartialEq)]
struct Found {
    /// Relative to the project's root, e.g. `CLAUDE.md` or `app/AGENTS.md`.
    path: String,
    text: String,
}

/// The project's instructions that `kind` wouldn't load itself, from the
/// folders between the chat's `cwd` and its repo's root (outermost first),
/// or `None` when it already has them all.
pub(super) fn for_chat(kind: AgentKind, strict: bool, cwd: &Path) -> Option<String> {
    let h = catalog::harness(kind);
    let native = match (&h.strict_instruction_files, strict) {
        (Some(files), true) => files,
        _ => &h.instruction_files,
    };
    section(&missing(cwd, native))
}

/// Each folder's instructions that a harness loading `native` files skips:
/// in a folder with any of them, it has that folder's own.
fn missing(cwd: &Path, native: &[String]) -> Vec<Found> {
    let folders = folders(cwd);
    let root = folders.last().cloned();
    let mut out = Vec::new();
    for dir in folders.iter().rev() {
        if native.iter().any(|f| dir.join(f).is_file()) {
            continue;
        }
        let Some((name, text)) = FILES.iter().find_map(|f| {
            let text = std::fs::read_to_string(dir.join(f)).ok()?;
            let text = text.trim();
            (!text.is_empty()).then(|| (*f, text.to_string()))
        }) else {
            continue;
        };
        let rel = root
            .as_deref()
            .and_then(|r| dir.strip_prefix(r).ok())
            .unwrap_or(Path::new(""));
        out.push(Found {
            path: rel.join(name).to_string_lossy().replace('\\', "/"),
            text,
        });
    }
    out
}

/// `cwd` and its parents up to the repo's root (the folder with `.git`),
/// innermost first. Outside a repo, only `cwd`: the folders above it are
/// someone's home, not the project.
fn folders(cwd: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for dir in cwd.ancestors() {
        out.push(dir.to_path_buf());
        if dir.join(".git").exists() {
            return out;
        }
    }
    vec![cwd.to_path_buf()]
}

fn section(found: &[Found]) -> Option<String> {
    if found.is_empty() {
        return None;
    }
    let mut out = String::from(
        "\n# The project's instructions\n\nThe repo you're working in keeps its instructions in files \
         your harness doesn't load, so tab passes them on. Follow them as you would your own \
         project instructions.\n",
    );
    let mut left = MAX;
    for f in found {
        if left == 0 {
            break;
        }
        let text: String = f.text.chars().take(left).collect();
        left -= text.chars().count();
        out.push_str(&format!("\n## From {}\n\n{text}\n", f.path));
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn repo(files: &[(&str, &str)]) -> PathBuf {
        let root = std::env::temp_dir().join(format!("tab-instr-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join(".git")).unwrap();
        for (path, text) in files {
            let p = root.join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
        root
    }

    fn native(files: &[&str]) -> Vec<String> {
        files.iter().map(|f| f.to_string()).collect()
    }

    #[test]
    fn a_claude_md_reaches_a_harness_that_reads_only_agents_md() {
        let root = repo(&[("CLAUDE.md", "Use pnpm.")]);
        let found = missing(&root, &native(&["AGENTS.md"]));
        assert_eq!(
            found,
            vec![Found {
                path: "CLAUDE.md".into(),
                text: "Use pnpm.".into()
            }]
        );
        assert!(
            section(&found)
                .unwrap()
                .contains("## From CLAUDE.md\n\nUse pnpm.")
        );
    }

    #[test]
    fn nothing_is_added_when_the_harness_loads_the_folders_file() {
        let root = repo(&[("CLAUDE.md", "Use pnpm."), ("AGENTS.md", "Use npm.")]);
        assert!(missing(&root, &native(&["AGENTS.md"])).is_empty());
        assert!(missing(&root, &native(&["CLAUDE.md", "AGENTS.md"])).is_empty());
        assert_eq!(section(&[]), None);
    }

    #[test]
    fn agents_md_comes_before_claude_md_and_gemini_md() {
        let root = repo(&[("GEMINI.md", "g"), ("CLAUDE.md", "c"), ("AGENTS.md", "a")]);
        assert_eq!(missing(&root, &native(&["X.md"]))[0].text, "a");
        let root = repo(&[("GEMINI.md", "g"), ("CLAUDE.md", "c")]);
        assert_eq!(missing(&root, &native(&["AGENTS.md"]))[0].text, "c");
    }

    #[test]
    fn folders_up_to_the_repo_root_count_outermost_first() {
        let root = repo(&[
            ("CLAUDE.md", "root"),
            ("app/CLAUDE.md", "app"),
            ("app/src/x", ""),
        ]);
        let found = missing(&root.join("app/src"), &native(&["AGENTS.md"]));
        let paths: Vec<_> = found.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["CLAUDE.md", "app/CLAUDE.md"]);
    }

    #[test]
    fn outside_a_repo_only_the_chats_folder_counts() {
        let outer = std::env::temp_dir().join(format!("tab-instr-{}", uuid::Uuid::new_v4()));
        let inner = outer.join("work");
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::write(outer.join("CLAUDE.md"), "home").unwrap();
        assert!(missing(&inner, &native(&["AGENTS.md"])).is_empty());
        std::fs::write(inner.join("CLAUDE.md"), "here").unwrap();
        assert_eq!(missing(&inner, &native(&["AGENTS.md"]))[0].text, "here");
    }

    #[test]
    fn empty_files_are_skipped_and_long_ones_cut() {
        let root = repo(&[("AGENTS.md", "  \n"), ("CLAUDE.md", "c")]);
        assert_eq!(missing(&root, &native(&["X.md"]))[0].text, "c");
        let long = "x".repeat(MAX + 10);
        let found = vec![Found {
            path: "CLAUDE.md".into(),
            text: long,
        }];
        let s = section(&found).unwrap();
        assert_eq!(s.matches('x').count(), MAX);
    }

    #[test]
    fn every_harness_says_which_files_it_loads() {
        for h in catalog::harnesses() {
            assert!(!h.instruction_files.is_empty(), "{}", h.id);
            for f in h
                .instruction_files
                .iter()
                .chain(h.strict_instruction_files.iter().flatten())
            {
                assert!(FILES.contains(&f.as_str()), "{}: {f}", h.id);
            }
        }
    }

    #[test]
    fn strict_opencode_no_longer_reads_claude_md() {
        let root = repo(&[("CLAUDE.md", "Use pnpm.")]);
        assert_eq!(for_chat(AgentKind::OpenCode, false, &root), None);
        assert!(for_chat(AgentKind::OpenCode, true, &root).is_some());
        assert_eq!(for_chat(AgentKind::ClaudeCode, true, &root), None);
        assert!(for_chat(AgentKind::Codex, false, &root).is_some());
    }
}
