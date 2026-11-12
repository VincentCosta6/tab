//! A repo's docs, read from its checkout on this computer: the READMEs and
//! other text at the top, and everything under `docs/`.

use std::path::Path;

use sha2::{Digest, Sha256};

use super::chunk::Doc;

const DOC_EXTS: &[&str] = &["md", "mdx", "markdown", "txt", "rst", "adoc"];
const DOC_DIRS: &[&str] = &["docs", "doc", "documentation", "guides"];
const SKIP_DIRS: &[&str] = &["node_modules", "target", "vendor", "dist", "build", ".git"];
const MAX_FILES: usize = 300;
const MAX_FILE: u64 = 256 * 1024;
const MAX_TOTAL: u64 = 8 * 1024 * 1024;

fn is_doc(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| DOC_EXTS.contains(&e.to_ascii_lowercase().as_str()))
}

/// The doc files in the checkout at `root`, as paths relative to it, sorted.
pub fn doc_files(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut total = 0u64;
    let mut take = |p: &Path, out: &mut Vec<String>| {
        let Ok(meta) = p.metadata() else { return };
        if !meta.is_file()
            || meta.len() > MAX_FILE
            || total + meta.len() > MAX_TOTAL
            || out.len() >= MAX_FILES
        {
            return;
        }
        if let Ok(rel) = p.strip_prefix(root) {
            total += meta.len();
            out.push(rel.to_string_lossy().replace('\\', "/"));
        }
    };
    let Ok(top) = std::fs::read_dir(root) else {
        return out;
    };
    let mut top: Vec<_> = top.flatten().map(|e| e.path()).collect();
    top.sort();
    for p in &top {
        if is_doc(p) {
            take(p, &mut out);
        }
    }
    for d in DOC_DIRS {
        let mut stack = vec![root.join(d)];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            let mut entries: Vec<_> = entries.flatten().collect();
            entries.sort_by_key(|e| std::cmp::Reverse(e.file_name()));
            for e in entries {
                let p = e.path();
                // Not following links keeps the walk inside the checkout.
                let Ok(ft) = e.file_type() else { continue };
                if ft.is_dir() {
                    let name = e.file_name();
                    let name = name.to_string_lossy();
                    if !name.starts_with('.') && !SKIP_DIRS.contains(&name.as_ref()) {
                        stack.push(p);
                    }
                } else if ft.is_file() && is_doc(&p) {
                    take(&p, &mut out);
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// A fingerprint of the files' names, sizes and times, to skip a checkout
/// whose docs haven't changed.
pub fn fingerprint(root: &Path, files: &[String]) -> String {
    let mut h = Sha256::new();
    for f in files {
        h.update(f.as_bytes());
        if let Ok(m) = root.join(f).metadata() {
            h.update(m.len().to_le_bytes());
            if let Ok(t) = m
                .modified()
                .map(|t| t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default())
            {
                h.update(t.as_millis().to_le_bytes());
            }
        }
    }
    format!("{:x}", h.finalize())
}

/// Where a file in `repo` is on GitHub, when `repo` is `owner/name`.
fn web_link(repo: &str, file: &str) -> Option<String> {
    let mut parts = repo.split('/');
    let (o, n) = (parts.next()?, parts.next()?);
    (parts.next().is_none() && !o.is_empty() && !n.is_empty() && !repo.contains(':'))
        .then(|| format!("https://github.com/{o}/{n}/blob/HEAD/{file}"))
}

/// The docs as documents, each cited by its GitHub link (or its path).
pub fn read_docs(root: &Path, repo: &str, files: &[String]) -> Vec<Doc> {
    files
        .iter()
        .filter_map(|f| {
            let text = std::fs::read(root.join(f)).ok()?;
            let text = String::from_utf8_lossy(&text).into_owned();
            (!text.trim().is_empty()).then(|| Doc {
                title: format!("{repo}: {f}"),
                source: web_link(repo, f).or_else(|| Some(root.join(f).display().to_string())),
                text,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_readmes_and_docs_but_not_dependencies() {
        let root = std::env::temp_dir().join(format!("tab-repo-docs-{}", uuid::Uuid::new_v4()));
        for (p, body) in [
            ("README.md", "# App"),
            ("main.rs", "fn main() {}"),
            ("docs/setup.md", "Run it."),
            ("docs/deep/more.mdx", "More."),
            ("docs/node_modules/x/README.md", "no"),
            ("src/notes.md", "not docs"),
        ] {
            let p = root.join(p);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        }
        let files = doc_files(&root);
        assert_eq!(
            files,
            vec!["README.md", "docs/deep/more.mdx", "docs/setup.md"]
        );
        let docs = read_docs(&root, "acme/app", &files);
        assert_eq!(
            docs[0].source.as_deref(),
            Some("https://github.com/acme/app/blob/HEAD/README.md")
        );
        assert_eq!(docs[0].title, "acme/app: README.md");
        let a = fingerprint(&root, &files);
        assert_eq!(a, fingerprint(&root, &files));
        std::fs::write(root.join("docs/setup.md"), "Run it twice.").unwrap();
        assert_ne!(a, fingerprint(&root, &files));
        std::fs::remove_dir_all(&root).unwrap();
    }
}
