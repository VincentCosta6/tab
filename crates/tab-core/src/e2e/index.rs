//! The vault's search index, on this device only: each entry's name,
//! note, field names (not values) and file text, in passages. Kept in
//! memory while the vault is open and saved sealed under a key derived
//! from the vault key (`vault-index.sealed`), so opening it again doesn't
//! re-read every file. Never in `tab.db`.
//!
//! A vault holds tens of entries, not thousands, so it's matched by words
//! rather than through the embedding index.

use std::collections::HashMap;
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::Body;
use super::crypto::{self, Key, Sealed};
use crate::model::ProjectResource;

const FILE: &str = "vault-index.sealed";
const AAD: &[u8] = b"tab vault index";
const PASSAGE: usize = 800;

#[derive(Default, Serialize, Deserialize)]
pub struct Index {
    docs: Vec<Doc>,
}

#[derive(Serialize, Deserialize)]
struct Doc {
    id: String,
    updated_at: DateTime<Utc>,
    passages: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct Saved {
    key_id: String,
    sealed: Sealed,
}

fn words(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 2)
        .map(str::to_lowercase)
        .collect()
}

fn passages(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for para in text.split("\n\n").flat_map(|p| p.lines()) {
        if cur.len() + para.len() > PASSAGE && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push('\n');
        }
        // A single long line is cut on a character boundary.
        let mut rest = para;
        while rest.len() > PASSAGE {
            let mut cut = PASSAGE;
            while !rest.is_char_boundary(cut) {
                cut -= 1;
            }
            out.push(rest[..cut].to_string());
            rest = &rest[cut..];
        }
        cur.push_str(rest);
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

impl Index {
    pub fn load(dir: &Path, keys: &HashMap<String, Key>) -> Option<Self> {
        let bytes = std::fs::read(dir.join(FILE)).ok()?;
        let saved: Saved = serde_json::from_slice(&bytes).ok()?;
        let key = keys.get(&saved.key_id)?.derive("index");
        let plain = crypto::open(&key, AAD, &saved.sealed).ok()?;
        serde_json::from_slice(&plain).ok()
    }

    pub fn save(&self, dir: &Path, key_id: &str, key: &Key) -> anyhow::Result<()> {
        let sealed = crypto::seal(
            &key.derive("index"),
            key_id,
            AAD,
            &serde_json::to_vec(self)?,
        );
        let tmp = dir.join(format!("{FILE}.tmp"));
        std::fs::write(
            &tmp,
            serde_json::to_vec(&Saved {
                key_id: key_id.into(),
                sealed,
            })?,
        )?;
        std::fs::rename(tmp, dir.join(FILE))?;
        Ok(())
    }

    /// Entries the index doesn't have as they are now.
    pub fn stale(&self, entries: &[ProjectResource]) -> Vec<ProjectResource> {
        entries
            .iter()
            .filter(|r| {
                !self
                    .docs
                    .iter()
                    .any(|d| d.id == r.id && d.updated_at == r.updated_at)
            })
            .cloned()
            .collect()
    }

    /// Drops what's gone and puts in what was read again.
    pub fn update(&mut self, entries: &[ProjectResource], read: Vec<(ProjectResource, String)>) {
        self.docs.retain(|d| {
            entries.iter().any(|r| r.id == d.id) && !read.iter().any(|(r, _)| r.id == d.id)
        });
        for (r, text) in read {
            self.docs.push(Doc {
                id: r.id,
                updated_at: r.updated_at,
                passages: passages(&text),
            });
        }
    }

    /// A file entry's whole text.
    pub fn text(&self, id: &str) -> Option<String> {
        self.docs
            .iter()
            .find(|d| d.id == id)
            .map(|d| d.passages.join("\n"))
            .filter(|t| !t.trim().is_empty())
    }

    /// The entries that match, best first, with their best passage. An
    /// empty query lists them all.
    pub fn search(
        &self,
        query: &str,
        entries: &[(ProjectResource, Body)],
    ) -> Vec<(usize, Option<String>)> {
        let q = words(query);
        let mut scored: Vec<(usize, usize, Option<String>)> = entries
            .iter()
            .enumerate()
            .filter_map(|(i, (r, b))| {
                if q.is_empty() {
                    return Some((i, 0, None));
                }
                let mut head = words(&b.title);
                for f in &b.fields {
                    head.extend(words(&f.label));
                }
                let in_head = q.iter().filter(|w| head.contains(w)).count();
                let best = self.docs.iter().find(|d| d.id == r.id).and_then(|d| {
                    d.passages
                        .iter()
                        .map(|p| {
                            let pw = words(p);
                            (q.iter().filter(|w| pw.contains(w)).count(), p)
                        })
                        .filter(|(n, _)| *n > 0)
                        .max_by_key(|(n, _)| *n)
                });
                let score = in_head * 2 + best.as_ref().map_or(0, |(n, _)| *n);
                (score > 0).then(|| (i, score, best.map(|(_, p)| p.clone())))
            })
            .collect();
        scored.sort_by_key(|(_, s, _)| std::cmp::Reverse(*s));
        scored
            .into_iter()
            .take(20)
            .map(|(i, _, p)| (i, p))
            .collect()
    }
}

/// Words with four or more digits in them: account numbers, tax ids.
pub fn numbers(text: &str) -> Vec<String> {
    text.split(|c: char| c.is_whitespace() || c == ',' || c == ';' || c == '(' || c == ')')
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()))
        .filter(|w| w.chars().filter(|c| c.is_ascii_digit()).count() >= 4)
        .map(str::to_string)
        .collect()
}

/// `text` with [`numbers`] masked.
pub fn mask_numbers(text: &str) -> String {
    let mut out = text.to_string();
    for n in numbers(text) {
        out = out.replace(&n, &crypto::mask(&n));
    }
    out
}

/// Whether an asked-for name means this label: "EIN" matches "EIN",
/// "tax id" matches "Federal tax ID".
pub fn matches(wanted: &str, label: &str) -> bool {
    let norm = |s: &str| words(s).join(" ");
    let (w, l) = (norm(wanted), norm(label));
    !w.is_empty()
        && !l.is_empty()
        && (w == l || (w.len() >= 3 && l.contains(&w)) || (l.len() >= 3 && w.contains(&l)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_numbers_and_matches_labels() {
        assert_eq!(
            numbers("EIN: 12-3456789, filed 2024"),
            vec!["12-3456789", "2024"]
        );
        assert_eq!(mask_numbers("EIN 12-3456789"), "EIN •••• 6789");
        assert!(matches("EIN", "ein"));
        assert!(matches("tax id", "Federal tax ID"));
        assert!(!matches("bank", "EIN"));
        assert!(
            passages(&"x".repeat(2000))
                .iter()
                .all(|p| p.len() <= PASSAGE)
        );
    }
}
