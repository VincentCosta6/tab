//! Vector search with sqlite-vec, in the same database file.
//!
//! Items and project notes are embedded in the background (see
//! `crate::embed`). Their vectors live in a `vec0` table whose width depends
//! on the model, so it's created at runtime rather than by a migration;
//! switching models drops and rebuilds it. Search fuses FTS5 keyword ranks
//! with vector ranks (reciprocal rank fusion), so exact names and
//! paraphrases both find things.

use std::collections::HashMap;
use std::sync::Once;

use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};

use super::items::get_item;
use super::{Scope, Store, ts};
use crate::model::{ItemState, ProjectNote, QueueItem};

/// Loads sqlite-vec into every connection opened after this.
pub(crate) fn register() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| unsafe {
        // SAFETY: the documented way to register sqlite-vec with rusqlite;
        // the entry point has the signature SQLite expects.
        #[allow(clippy::missing_transmute_annotations)]
        rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute(
            sqlite_vec::sqlite3_vec_init as *const (),
        )));
    });
}

/// One thing to embed: `item:<id>` or `note:<id>`, and its text.
#[derive(Debug, Clone)]
pub struct Doc {
    pub key: String,
    pub text: String,
}

fn item_text(i: &QueueItem) -> String {
    let e = &i.event;
    let mut t = e.title.clone();
    for part in [&e.actor, &e.context].into_iter().flatten() {
        t.push_str(" · ");
        t.push_str(part);
    }
    if let Some(b) = &e.body {
        t.push('\n');
        t.extend(b.chars().take(2000));
    }
    t
}

fn to_blob(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

impl Store {
    /// Makes the vector table ready for `model` with `dim` dimensions. A
    /// different model than last time starts the index over.
    pub fn vectors_prepare(&self, model: &str, dim: usize) -> anyhow::Result<()> {
        let conn = self.conn();
        let current: Option<(String, i64)> = conn
            .query_row("SELECT model, dim FROM embed_model WHERE id = 1", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()?;
        if current
            .as_ref()
            .is_some_and(|(m, d)| m == model && *d == dim as i64)
        {
            return Ok(());
        }
        conn.execute_batch(&format!(
            "DROP TABLE IF EXISTS doc_vec;
             DELETE FROM doc_embeddings;
             CREATE VIRTUAL TABLE doc_vec USING vec0(doc TEXT PRIMARY KEY, embedding float[{dim}] distance_metric=cosine);"
        ))?;
        conn.execute(
            "INSERT INTO embed_model (id, model, dim) VALUES (1, ?1, ?2)
             ON CONFLICT (id) DO UPDATE SET model = excluded.model, dim = excluded.dim",
            params![model, dim as i64],
        )?;
        Ok(())
    }

    /// Up to `limit` items and notes that are new or changed since they
    /// were last embedded, most recent first.
    pub fn vectors_pending(&self, limit: usize) -> anyhow::Result<Vec<Doc>> {
        let conn = self.conn();
        let item_ids: Vec<String> = conn
            .prepare(
                "SELECT i.id FROM items i LEFT JOIN doc_embeddings d ON d.doc = 'item:' || i.id
                 WHERE d.doc IS NULL OR d.embedded_at < i.updated_at
                 ORDER BY i.occurred_at DESC LIMIT ?1",
            )?
            .query_map([limit], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        let mut out = Vec::new();
        for id in item_ids {
            if let Some(i) = get_item(&conn, &id)? {
                out.push(Doc {
                    key: format!("item:{id}"),
                    text: item_text(&i),
                });
            }
        }
        let notes: Vec<(String, String, String)> = conn
            .prepare(
                "SELECT n.id, n.topic, n.text FROM project_notes n LEFT JOIN doc_embeddings d ON d.doc = 'note:' || n.id
                 WHERE d.doc IS NULL OR d.embedded_at < n.updated_at LIMIT ?1",
            )?
            .query_map([limit], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<_, _>>()?;
        out.extend(notes.into_iter().map(|(id, topic, text)| Doc {
            key: format!("note:{id}"),
            text: format!("{topic}: {text}"),
        }));
        // Drop vectors whose item or note is gone.
        conn.execute_batch(
            "DELETE FROM doc_vec WHERE doc IN (SELECT doc FROM doc_embeddings d WHERE
                 (d.doc LIKE 'item:%' AND NOT EXISTS (SELECT 1 FROM items WHERE 'item:' || items.id = d.doc)) OR
                 (d.doc LIKE 'note:%' AND NOT EXISTS (SELECT 1 FROM project_notes n WHERE 'note:' || n.id = d.doc)));
             DELETE FROM doc_embeddings WHERE
                 (doc LIKE 'item:%' AND NOT EXISTS (SELECT 1 FROM items WHERE 'item:' || items.id = doc)) OR
                 (doc LIKE 'note:%' AND NOT EXISTS (SELECT 1 FROM project_notes n WHERE 'note:' || n.id = doc));",
        )?;
        Ok(out)
    }

    pub fn vectors_store(
        &self,
        docs: &[(String, Vec<f32>)],
        now: DateTime<Utc>,
    ) -> anyhow::Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        for (doc, v) in docs {
            // vec0 has no upsert.
            tx.execute("DELETE FROM doc_vec WHERE doc = ?1", [doc])?;
            tx.execute(
                "INSERT INTO doc_vec (doc, embedding) VALUES (?1, ?2)",
                params![doc, to_blob(v)],
            )?;
            tx.execute(
                "INSERT INTO doc_embeddings (doc, embedded_at) VALUES (?1, ?2)
                 ON CONFLICT (doc) DO UPDATE SET embedded_at = excluded.embedded_at",
                params![doc, ts(now)],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// How many docs have vectors, and how many are waiting.
    pub fn vectors_status(&self) -> anyhow::Result<(i64, i64)> {
        let conn = self.conn();
        let done: i64 = conn.query_row("SELECT COUNT(*) FROM doc_embeddings", [], |r| r.get(0))?;
        let total: i64 = conn.query_row(
            "SELECT (SELECT COUNT(*) FROM items) + (SELECT COUNT(*) FROM project_notes)",
            [],
            |r| r.get(0),
        )?;
        Ok((done, (total - done).max(0)))
    }

    /// Nearest docs to `v`, closest first, as (doc key, cosine distance).
    fn knn(&self, v: &[f32], k: usize) -> anyhow::Result<Vec<(String, f64)>> {
        let conn = self.conn();
        let exists: bool = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE name = 'doc_vec'",
                [],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);
        if !exists {
            return Ok(Vec::new());
        }
        let out = conn
            .prepare("SELECT doc, distance FROM doc_vec WHERE embedding MATCH ?1 AND k = ?2 ORDER BY distance")?
            .query_map(params![to_blob(v), k as i64], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        Ok(out)
    }

    /// Keyword and meaning search over items, fused. Without a query vector
    /// (no model yet) this is the keyword search.
    pub fn search_hybrid(
        &self,
        query: &str,
        query_vec: Option<&[f32]>,
        states: &[ItemState],
        scope: &Scope,
        limit: usize,
    ) -> anyhow::Result<Vec<QueueItem>> {
        let keyword = self.search_in(query, states, scope, limit * 2)?;
        let Some(v) = query_vec else {
            return Ok(keyword.into_iter().take(limit).collect());
        };
        // Reciprocal rank fusion: robust to the two scores being on
        // different scales. k = 60 is the usual constant.
        let mut score: HashMap<String, f64> = HashMap::new();
        let mut items: HashMap<String, QueueItem> = HashMap::new();
        for (rank, i) in keyword.into_iter().enumerate() {
            *score.entry(i.id.clone()).or_default() += 1.0 / (60.0 + rank as f64);
            items.insert(i.id.clone(), i);
        }
        // A narrow scope filters out most neighbours, so look further.
        let narrow = scope.since.is_some() || scope.until.is_some() || scope.project_id.is_some();
        let near = self.knn(v, if narrow { limit * 20 } else { limit * 4 })?;
        let mut rank = 0usize;
        for (doc, distance) in near {
            let Some(id) = doc.strip_prefix("item:") else {
                continue;
            };
            // Past this distance it's noise, not meaning.
            if distance > 0.75 {
                break;
            }
            let item = match items.get(id) {
                Some(i) => i.clone(),
                None => match self.get(id)? {
                    Some(i) => i,
                    None => continue,
                },
            };
            if !states.contains(&item.state) || !scope.contains(&item) {
                continue;
            }
            *score.entry(id.to_string()).or_default() += 1.0 / (60.0 + rank as f64);
            items.insert(id.to_string(), item);
            rank += 1;
        }
        let mut ranked: Vec<_> = score.into_iter().collect();
        ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        Ok(ranked
            .into_iter()
            .take(limit)
            .filter_map(|(id, _)| items.remove(&id))
            .collect())
    }

    /// Project notes closest in meaning to the query.
    pub fn notes_near(&self, query_vec: &[f32], limit: usize) -> anyhow::Result<Vec<ProjectNote>> {
        let near = self.knn(query_vec, limit * 3)?;
        let mut out = Vec::new();
        for (doc, distance) in near {
            let Some(id) = doc.strip_prefix("note:") else {
                continue;
            };
            if distance > 0.7 || out.len() >= limit {
                break;
            }
            out.extend(self.note_get(id)?);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::IncomingEvent;
    use crate::store::Upsert;

    /// A toy embedding: counts of a few words, so "money" lands near "billing".
    fn embed(t: &str) -> Vec<f32> {
        let t = t.to_lowercase();
        let has = |w: &[&str]| w.iter().filter(|x| t.contains(*x)).count() as f32;
        vec![
            has(&["billing", "invoice", "money", "payment"]),
            has(&["login", "auth", "password"]),
            has(&["deploy", "release"]),
            0.1,
        ]
    }

    #[test]
    fn hybrid_search_finds_by_meaning_and_keyword() {
        let s = Store::in_memory().unwrap();
        let now = Utc::now();
        s.vectors_prepare("toy", 4).unwrap();
        let mk = |id: &str, title: &str| {
            let Upsert::Inserted(i) = s
                .upsert(
                    &IncomingEvent {
                        source: "t".into(),
                        external_id: id.into(),
                        kind: "t.k".into(),
                        title: title.into(),
                        occurred_at: now,
                        ..Default::default()
                    },
                    now,
                )
                .unwrap()
            else {
                panic!()
            };
            i
        };
        let inv = mk("1", "Invoice totals are off by a cent");
        let auth = mk("2", "Password reset email never arrives");
        let pending = s.vectors_pending(10).unwrap();
        assert_eq!(pending.len(), 2);
        let vecs: Vec<_> = pending
            .iter()
            .map(|d| (d.key.clone(), embed(&d.text)))
            .collect();
        s.vectors_store(&vecs, Utc::now()).unwrap();
        assert!(s.vectors_pending(10).unwrap().is_empty());
        assert_eq!(s.vectors_status().unwrap(), (2, 0));

        // No shared words with "money", found by meaning.
        let q = embed("where did the money go");
        let all = Scope::default();
        let hits = s
            .search_hybrid(
                "where did the money go",
                Some(&q),
                &[ItemState::Open],
                &all,
                5,
            )
            .unwrap();
        assert_eq!(hits.first().map(|i| &i.id), Some(&inv.id));
        // Exact words still work, and without vectors it's keyword search.
        let hits = s
            .search_hybrid("Password", None, &[ItemState::Open], &all, 5)
            .unwrap();
        assert_eq!(hits[0].id, auth.id);
        // A window that ends before they happened finds nothing.
        let old = Scope {
            until: Some(now - chrono::Duration::days(1)),
            ..Default::default()
        };
        assert!(
            s.search_hybrid(
                "where did the money go",
                Some(&q),
                &[ItemState::Open],
                &old,
                5
            )
            .unwrap()
            .is_empty()
        );
        assert!(
            s.search_hybrid("Password", None, &[ItemState::Open], &old, 5)
                .unwrap()
                .is_empty()
        );

        // A changed item is embedded again.
        s.upsert(
            &IncomingEvent {
                source: "t".into(),
                external_id: "1".into(),
                kind: "t.k".into(),
                title: "Invoice totals fixed".into(),
                occurred_at: now,
                ..Default::default()
            },
            Utc::now() + chrono::Duration::seconds(1),
        )
        .unwrap();
        assert_eq!(s.vectors_pending(10).unwrap().len(), 1);

        // Switching models starts over.
        s.vectors_prepare("other", 4).unwrap();
        assert_eq!(s.vectors_pending(10).unwrap().len(), 2);
    }
}
