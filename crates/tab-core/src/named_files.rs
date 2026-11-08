//! Files an agent's reply names: a screenshot it took, a report it wrote.
//!
//! The reply only says a path on the computer the agent ran on, which other
//! devices don't have. So when the reply is stored, each local file it names
//! goes into the hub too, and the message's `files` record which hash each
//! path had ([`FileRef::path`]). Another device showing that path looks it
//! up there ([`crate::store::Store::named_file`]) and fetches it.
//!
//! What counts as naming a file matches what the chat shows as one
//! (`filesInText` in `src/lib/threadFiles.ts`): a markdown image or link to
//! a local path, or a path to a file in backticks. Fenced code blocks don't.

use std::path::{Path, PathBuf};

use crate::live::FileRef;

/// Largest file shared this way; bigger ones stay on their computer.
pub const MAX_NAMED: u64 = 30 * 1024 * 1024;
/// Most files one reply shares.
const MAX_PER_REPLY: usize = 12;

/// Local paths `text` names, each once, in order. Relative ones are taken
/// against `base`, the folder the agent works in.
pub fn paths_in_text(text: &str, base: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    let mut push = |p: String| {
        if !out.contains(&p) {
            out.push(p);
        }
    };
    let text = text.replace("\r\n", "\n");
    for block in text.split("\n\n") {
        if block.trim_start().starts_with("```") {
            continue;
        }
        let b = block.as_bytes();
        let mut i = 0;
        while i < b.len() {
            match b[i] {
                b'`' => {
                    let Some(end) = block[i + 1..].find('`') else {
                        break;
                    };
                    let code = block[i + 1..i + 1 + end].trim();
                    if !code.contains(char::is_whitespace)
                        && looks_like_file(code)
                        && let Some(p) = resolve(code, base)
                    {
                        push(p);
                    }
                    i += end + 2;
                }
                b'[' | b'!' => {
                    let start = if b[i] == b'!' { i + 1 } else { i };
                    if b.get(start) != Some(&b'[') {
                        i += 1;
                        continue;
                    }
                    let Some(close) = block[start..].find("](") else {
                        i += 1;
                        continue;
                    };
                    let target_at = start + close + 2;
                    let Some(end) = block[target_at..].find(')') else {
                        i += 1;
                        continue;
                    };
                    let target = &block[target_at..target_at + end];
                    let target = target.strip_prefix("file://").unwrap_or(target);
                    if !target.contains(char::is_whitespace)
                        && !target.contains("://")
                        && looks_like_file(target)
                        && let Some(p) = resolve(target, base)
                    {
                        push(p);
                    }
                    i = target_at + end + 1;
                }
                _ => i += 1,
            }
        }
    }
    out
}

/// Has an extension, as a file does (not a folder or a command).
fn looks_like_file(s: &str) -> bool {
    let name = s.rsplit(['/', '\\']).next().unwrap_or(s);
    match name.rsplit_once('.') {
        Some((stem, ext)) => {
            !stem.is_empty()
                && (1..=8).contains(&ext.len())
                && ext.bytes().all(|c| c.is_ascii_alphanumeric())
                && !s.contains(['<', '>', '|'])
        }
        None => false,
    }
}

fn is_local(s: &str) -> bool {
    s.starts_with('/')
        || s.starts_with("~/")
        || (s.len() > 2 && s.as_bytes()[1] == b':' && matches!(s.as_bytes()[2], b'\\' | b'/'))
}

fn resolve(p: &str, base: Option<&str>) -> Option<String> {
    if let Some(rest) = p.strip_prefix("~/") {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .ok()?;
        return Some(Path::new(&home).join(rest).display().to_string());
    }
    if is_local(p) {
        return Some(p.to_string());
    }
    // A relative path only counts when it has a folder in it, as the chat
    // treats a bare `name.ext` as code, not a file.
    let base = base.filter(|b| is_local(b))?;
    if !p.contains(['/', '\\']) {
        return None;
    }
    let rel = p.strip_prefix("./").unwrap_or(p);
    Some(PathBuf::from(base).join(rel).display().to_string())
}

/// Puts each existing file `text` names (up to [`MAX_NAMED`] bytes) into
/// the hub through `put`, which returns its hash. Returns what to record in
/// the message's `files`.
pub fn share(
    text: &str,
    base: Option<&str>,
    mut put: impl FnMut(&[u8]) -> anyhow::Result<String>,
) -> Vec<FileRef> {
    let mut out = vec![];
    for path in paths_in_text(text, base) {
        if out.len() >= MAX_PER_REPLY {
            break;
        }
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if !meta.is_file() || meta.len() > MAX_NAMED {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        match put(&bytes) {
            Ok(hash) => out.push(FileRef {
                hash,
                name: crate::live::safe_name(&path),
                size: bytes.len() as u64,
                path: Some(path),
            }),
            Err(e) => tracing::warn!(error = %e, "couldn't put a file an agent named in the hub"),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_files_a_reply_names() {
        let text = "Here it is: `/tmp/a/shot.png` and ![chart](/tmp/b/chart.png).\n\n\
                    See [the report](out/report.pdf), not `npm test` or `src`.\n\n\
                    ```\n`/tmp/in/code.png`\n```";
        assert_eq!(
            paths_in_text(text, Some("/work")),
            vec![
                "/tmp/a/shot.png",
                "/tmp/b/chart.png",
                "/work/out/report.pdf"
            ]
        );
    }

    #[test]
    fn skips_urls_and_bare_names() {
        let text = "[site](https://example.com/a.png) `notes.md` `/tmp/x.png` `/tmp/x.png`";
        assert_eq!(paths_in_text(text, Some("/work")), vec!["/tmp/x.png"]);
    }

    #[test]
    fn shares_files_that_exist() {
        let dir = std::env::temp_dir().join(format!("tab-named-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let png = dir.join("shot.png");
        std::fs::write(&png, b"png bytes").unwrap();
        let text = format!("`{}` and `/nope/missing.png`", png.display());
        let refs = share(&text, None, |b| Ok(tab_domain::blob::hash(b)));
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].hash, tab_domain::blob::hash(b"png bytes"));
        assert_eq!(refs[0].name, "shot.png");
        assert_eq!(refs[0].path.as_deref(), Some(png.to_str().unwrap()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
