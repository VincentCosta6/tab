//! The index of what projects know: their resources and repos' docs, cut
//! into pieces (`resource_chunks`), searchable by keyword (FTS5) and by
//! meaning (`doc:res:<id>:<n>` in `doc_vec`). Built on each device and
//! never synced; only a resource's `status` and `chunks` travel.
//!
//! Who reads what is decided here, from the resource's current access level:
//! a project's coordinator reads its `coordinator` pieces, every other
//! reader only `all` ones. Vault resources are never indexed.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};

use super::{Store, ts};
use crate::knowledge::chunk::Piece;

/// Something to index: a project resource, or a repo's docs (`kind` "repo").
#[derive(Debug, Clone, PartialEq)]
pub struct IndexTarget {
    pub id: String,
    pub project_id: String,
    pub kind: String,
    pub title: String,
    pub url: Option<String>,
    pub note: String,
    pub access: String,
    pub options: serde_json::Value,
    /// The resource's `updated_at`; a change indexes it again.
    pub version: String,
    /// A repo's checkout on this computer.
    pub path: Option<String>,
}

/// Who is searching, which decides the pieces they may read.
#[derive(Debug, Clone, Default)]
pub struct Reader {
    /// The project this session coordinates (from its own key).
    pub coordinator_of: Option<String>,
    /// Only this project's pieces.
    pub project: Option<String>,
}

/// One piece search found, with what to cite.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ChunkHit {
    pub resource_id: String,
    pub project_id: String,
    /// The resource's name (or the repo's).
    pub resource: String,
    pub kind: String,
    /// The page, file or email it's from.
    pub title: String,
    pub source: Option<String>,
    pub text: String,
    pub access: String,
}

/// Joins a piece to what it belongs to and keeps only what `?1` (the
/// coordinated project) and `?2` (a project filter) allow.
/// A compacted stretch of a coordinator's chat (`session_segments`) is
/// searchable like a coordinator-only resource of its project.
const READABLE: &str = "FROM resource_chunks c
     LEFT JOIN project_resources r ON r.id = c.resource_id
     LEFT JOIN project_repos p ON p.id = c.resource_id
     LEFT JOIN session_segments g ON g.id = c.resource_id
     WHERE (r.id IS NOT NULL OR p.id IS NOT NULL OR g.id IS NOT NULL)
       AND (COALESCE(r.access, IIF(g.id IS NULL, 'all', 'coordinator')) = 'all'
            OR (COALESCE(r.access, IIF(g.id IS NULL, NULL, 'coordinator')) = 'coordinator'
                AND c.project_id = ?1))
       AND (?2 IS NULL OR c.project_id = ?2)";

/// The joins [`HIT_COLS`] reads from, for queries that select from `c` alone.
const HIT_JOINS: &str = "LEFT JOIN project_resources r ON r.id = c.resource_id
     LEFT JOIN project_repos p ON p.id = c.resource_id
     LEFT JOIN session_segments g ON g.id = c.resource_id";

const HIT_COLS: &str = "c.id, c.resource_id, c.project_id,
     COALESCE(r.title, p.repo, 'Coordinator history') AS resource,
     COALESCE(r.kind, IIF(g.id IS NULL, 'repo', 'history')) AS kind, c.title, c.source, c.text,
     COALESCE(r.access, IIF(g.id IS NULL, 'all', 'coordinator')) AS access";

fn hit_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<(i64, ChunkHit)> {
    Ok((
        r.get("id")?,
        ChunkHit {
            resource_id: r.get("resource_id")?,
            project_id: r.get("project_id")?,
            resource: r.get("resource")?,
            kind: r.get("kind")?,
            title: r.get("title")?,
            source: r.get("source")?,
            text: r.get("text")?,
            access: r.get("access")?,
        },
    ))
}

/// The words of a query for FTS5, any of which may match (trigram needs
/// three characters).
fn fts_words(q: &str) -> Option<String> {
    let words: Vec<String> = q
        .split(|c: char| !c.is_alphanumeric() && c != '-' && c != '_' && c != '.' && c != '@')
        .filter(|w| w.chars().count() >= 3)
        .take(12)
        .map(|w| format!("\"{}\"", w.replace('"', "\"\"")))
        .collect();
    (!words.is_empty()).then(|| words.join(" OR "))
}

fn has_vec(conn: &rusqlite::Connection) -> rusqlite::Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE name = 'doc_vec'",
            [],
            |_| Ok(true),
        )
        .optional()?
        .unwrap_or(false))
}

/// Drops a resource's pieces and their vectors.
fn drop_chunks(conn: &rusqlite::Connection, id: &str) -> rusqlite::Result<()> {
    if has_vec(conn)? {
        conn.execute(
            "DELETE FROM doc_vec WHERE doc IN (SELECT doc FROM doc_embeddings WHERE doc LIKE 'doc:res:' || ?1 || ':%')",
            [id],
        )?;
    }
    conn.execute(
        "DELETE FROM doc_embeddings WHERE doc LIKE 'doc:res:' || ?1 || ':%'",
        [id],
    )?;
    conn.execute("DELETE FROM resource_chunks WHERE resource_id = ?1", [id])?;
    Ok(())
}

/// Sets a resource's synced status and count, only when they change (so
/// devices that agree don't send each other the same row).
fn set_status(
    conn: &rusqlite::Connection,
    id: &str,
    status: &str,
    chunks: Option<i64>,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE project_resources SET status = ?2, chunks = COALESCE(?3, chunks)
         WHERE id = ?1 AND (status != ?2 OR chunks != COALESCE(?3, chunks))",
        params![id, status, chunks],
    )?;
    Ok(())
}

impl Store {
    pub fn knowledge_wake(&self) -> std::sync::Arc<tokio::sync::Notify> {
        self.knowledge_wake.clone()
    }

    /// Resources and repos with a checkout here that are new, changed, or
    /// due for another look, oldest change first. Also forgets pieces of
    /// resources that are gone.
    pub fn knowledge_due(
        &self,
        now: DateTime<Utc>,
        limit: usize,
    ) -> anyhow::Result<Vec<IndexTarget>> {
        let conn = self.conn();
        let gone: Vec<String> = conn
            .prepare(
                "SELECT resource_id FROM resource_index i WHERE
                     NOT EXISTS (SELECT 1 FROM project_resources r WHERE r.id = i.resource_id AND r.access != 'vault')
                     AND NOT EXISTS (SELECT 1 FROM project_repos p WHERE p.id = i.resource_id)
                 UNION SELECT DISTINCT resource_id FROM resource_chunks c WHERE
                     NOT EXISTS (SELECT 1 FROM project_resources r WHERE r.id = c.resource_id AND r.access != 'vault')
                     AND NOT EXISTS (SELECT 1 FROM project_repos p WHERE p.id = c.resource_id)",
            )?
            .query_map([], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        for id in gone {
            drop_chunks(&conn, &id)?;
            conn.execute("DELETE FROM resource_index WHERE resource_id = ?1", [&id])?;
        }
        let due = "(i.resource_id IS NULL OR i.seen_version != v.version OR (i.next_at IS NOT NULL AND i.next_at <= ?1))";
        let out = conn
            .prepare(&format!(
                "SELECT v.* FROM (
                     SELECT id, project_id, kind, title, url, note, access, options, updated_at AS version, NULL AS path
                     FROM project_resources WHERE kind IN ('website', 'file', 'email', 'note') AND access IN ('all', 'coordinator')
                     UNION ALL
                     SELECT id, project_id, 'repo', repo, NULL, note, 'all', '{{}}', updated_at, path
                     FROM project_repos WHERE path IS NOT NULL AND path != ''
                 ) v LEFT JOIN resource_index i ON i.resource_id = v.id
                 WHERE {due} ORDER BY v.version LIMIT ?2"
            ))?
            .query_map(params![ts(now), limit as i64], |r| {
                Ok(IndexTarget {
                    id: r.get("id")?,
                    project_id: r.get("project_id")?,
                    kind: r.get("kind")?,
                    title: r.get("title")?,
                    url: r.get("url")?,
                    note: r.get("note")?,
                    access: r.get("access")?,
                    options: serde_json::from_str(&r.get::<_, String>("options")?).unwrap_or_default(),
                    version: r.get("version")?,
                    path: r.get("path")?,
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(out)
    }

    /// The fingerprint of what was read last time.
    pub fn knowledge_fingerprint(&self, id: &str) -> anyhow::Result<Option<String>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT fingerprint FROM resource_index WHERE resource_id = ?1",
                [id],
                |r| r.get(0),
            )
            .optional()?
            .flatten())
    }

    /// Replaces a resource's pieces with `pieces` (read with `fingerprint`)
    /// and marks it indexed; next look at `next_at`.
    pub fn knowledge_put(
        &self,
        t: &IndexTarget,
        fingerprint: &str,
        pieces: &[Piece],
        next_at: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
    ) -> anyhow::Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        drop_chunks(&tx, &t.id)?;
        {
            let mut ins = tx.prepare(
                "INSERT INTO resource_chunks (resource_id, n, project_id, access, title, source, text)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            for (n, p) in pieces.iter().enumerate() {
                ins.execute(params![
                    t.id,
                    n as i64,
                    t.project_id,
                    t.access,
                    p.title,
                    p.source,
                    p.text
                ])?;
            }
        }
        tx.execute(
            "INSERT INTO resource_index (resource_id, seen_version, fingerprint, indexed_at, next_at, error)
             VALUES (?1, ?2, ?3, ?4, ?5, NULL)
             ON CONFLICT (resource_id) DO UPDATE SET seen_version = excluded.seen_version,
                 fingerprint = excluded.fingerprint, indexed_at = excluded.indexed_at,
                 next_at = excluded.next_at, error = NULL",
            params![t.id, t.version, fingerprint, ts(now), next_at.map(ts)],
        )?;
        set_status(&tx, &t.id, "indexed", Some(pieces.len() as i64))?;
        tx.commit()?;
        Ok(())
    }

    /// Read again and nothing changed: keeps the pieces, sets the next look.
    pub fn knowledge_unchanged(
        &self,
        t: &IndexTarget,
        next_at: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
    ) -> anyhow::Result<()> {
        let conn = self.conn();
        conn.execute(
            "UPDATE resource_index SET seen_version = ?2, indexed_at = ?3, next_at = ?4, error = NULL
             WHERE resource_id = ?1",
            params![t.id, t.version, ts(now), next_at.map(ts)],
        )?;
        // Pieces copy the resource's level; keep them in step after an edit.
        conn.execute(
            "UPDATE resource_chunks SET access = ?2 WHERE resource_id = ?1 AND access != ?2",
            params![t.id, t.access],
        )?;
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM resource_chunks WHERE resource_id = ?1",
            [&t.id],
            |r| r.get(0),
        )?;
        set_status(&conn, &t.id, "indexed", Some(n))?;
        Ok(())
    }

    /// Reading failed: `status` is `failed` or `needs_sign_in`. What was
    /// indexed before stays searchable. Tries again at `retry_at`.
    pub fn knowledge_failed(
        &self,
        t: &IndexTarget,
        status: &str,
        error: &str,
        retry_at: DateTime<Utc>,
    ) -> anyhow::Result<()> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO resource_index (resource_id, seen_version, next_at, error) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (resource_id) DO UPDATE SET seen_version = excluded.seen_version,
                 next_at = excluded.next_at, error = excluded.error",
            params![t.id, t.version, ts(retry_at), error],
        )?;
        set_status(&conn, &t.id, status, None)?;
        Ok(())
    }

    /// Reads a resource again on the next pass.
    pub fn knowledge_refresh(&self, id: &str) -> anyhow::Result<()> {
        self.conn().execute(
            "UPDATE resource_index SET next_at = '', fingerprint = NULL WHERE resource_id = ?1",
            [id],
        )?;
        self.knowledge_wake.notify_one();
        Ok(())
    }

    /// Notes when agents' searches returned pieces of these resources.
    pub fn knowledge_mark_used(&self, ids: &[&str], now: DateTime<Utc>) -> anyhow::Result<()> {
        let conn = self.conn();
        for id in ids {
            conn.execute(
                "UPDATE resource_index SET last_used_at = ?2 WHERE resource_id = ?1",
                params![id, ts(now)],
            )?;
        }
        Ok(())
    }

    /// Pieces of project resources that match `query`, by keyword and (with
    /// `query_vec`) by meaning, fused; only what `reader` may read.
    pub fn chunks_search(
        &self,
        query: &str,
        query_vec: Option<&[f32]>,
        reader: &Reader,
        limit: usize,
    ) -> anyhow::Result<Vec<ChunkHit>> {
        let near = match query_vec {
            Some(v) => self.knn(v, (limit * 20).min(400))?,
            None => Vec::new(),
        };
        let conn = self.conn();
        let mut score: HashMap<i64, f64> = HashMap::new();
        let mut hits: HashMap<i64, ChunkHit> = HashMap::new();
        if let Some(m) = fts_words(query) {
            let found: Vec<(i64, ChunkHit)> = conn
                .prepare(&format!(
                    "SELECT {HIT_COLS} FROM resource_chunks_fts f JOIN ({}) c ON c.id = f.rowid {HIT_JOINS}
                     WHERE resource_chunks_fts MATCH ?3
                     ORDER BY bm25(resource_chunks_fts, 3.0, 1.0) LIMIT ?4",
                    format_args!("SELECT c.* {READABLE}")
                ))?
                .query_map(
                    params![reader.coordinator_of, reader.project, m, (limit * 3) as i64],
                    hit_row,
                )?
                .collect::<Result<_, _>>()?;
            for (rank, (id, h)) in found.into_iter().enumerate() {
                *score.entry(id).or_default() += 1.0 / (60.0 + rank as f64);
                hits.insert(id, h);
            }
        }
        let mut one = conn.prepare(&format!(
            "SELECT {HIT_COLS} {READABLE} AND c.resource_id = ?3 AND c.n = ?4"
        ))?;
        let mut rank = 0usize;
        for (doc, distance) in near {
            let Some((rid, n)) = doc
                .strip_prefix("doc:res:")
                .and_then(|d| d.rsplit_once(':'))
            else {
                continue;
            };
            if distance > 0.75 || rank >= limit * 3 {
                break;
            }
            let Some((id, h)) = one
                .query_row(
                    params![
                        reader.coordinator_of,
                        reader.project,
                        rid,
                        n.parse::<i64>().unwrap_or(-1)
                    ],
                    hit_row,
                )
                .optional()?
            else {
                continue;
            };
            *score.entry(id).or_default() += 1.0 / (60.0 + rank as f64);
            hits.insert(id, h);
            rank += 1;
        }
        let mut ranked: Vec<_> = score.into_iter().collect();
        ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        Ok(ranked
            .into_iter()
            .take(limit)
            .filter_map(|(id, _)| hits.remove(&id))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::ResourceInput;

    fn piece(text: &str) -> Piece {
        Piece {
            title: "Page".into(),
            source: Some("https://acme.dev/p".into()),
            text: text.into(),
        }
    }

    #[test]
    fn indexes_due_resources_and_searches_by_level() {
        let s = Store::in_memory().unwrap();
        let now = Utc::now();
        let a = s.project_save(None, "Acme", "", "", now).unwrap();
        let b = s.project_save(None, "Bolt", "", "", now).unwrap();
        let add = |p: &str, kind: &str, url: &str, access: &str| {
            s.resource_save(
                p,
                &ResourceInput {
                    kind: kind.into(),
                    title: url.into(),
                    url: Some(url.into()),
                    access: Some(access.into()),
                    ..Default::default()
                },
                "user",
                now,
            )
            .unwrap()
        };
        let site = add(&a.id, "website", "https://acme.dev", "all");
        let billing = add(&a.id, "email", "label:billing", "coordinator");
        let other = add(&b.id, "website", "https://bolt.dev", "all");
        add(&a.id, "service", "https://dash.cloudflare.com/x", "all");
        s.repo_save(&a.id, "acme/app", Some("/tmp/acme"), None, "user", now)
            .unwrap();
        s.repo_save(&a.id, "acme/elsewhere", None, None, "user", now)
            .unwrap();

        let due = s.knowledge_due(now, 50).unwrap();
        let kinds: Vec<_> = due.iter().map(|t| t.kind.as_str()).collect();
        assert_eq!(due.len(), 4, "{kinds:?}");
        assert!(kinds.contains(&"repo") && !kinds.contains(&"service"));
        let t = |id: &str| due.iter().find(|t| t.id == id).unwrap().clone();

        let later = now + chrono::Duration::hours(1);
        s.knowledge_put(
            &t(&site.id),
            "f1",
            &[
                piece("Refunds within thirty days"),
                piece("Pricing: Pro is $29"),
            ],
            Some(later),
            now,
        )
        .unwrap();
        s.knowledge_put(
            &t(&billing.id),
            "f2",
            &[piece("Invoice 4411 refunded to Dana")],
            Some(later),
            now,
        )
        .unwrap();
        s.knowledge_put(
            &t(&other.id),
            "f3",
            &[piece("Bolt refunds take a week")],
            None,
            now,
        )
        .unwrap();
        let rs = s.project_resources(&a.id, "coordinator").unwrap();
        let st = rs.iter().find(|r| r.id == site.id).unwrap();
        assert_eq!((st.status.as_str(), st.chunks), ("indexed", 2));
        assert!(st.indexed_at.is_some());
        assert_eq!(
            s.knowledge_fingerprint(&site.id).unwrap().as_deref(),
            Some("f1")
        );

        // Only the repo is still due, until the schedule comes round.
        let due: Vec<_> = s
            .knowledge_due(now, 50)
            .unwrap()
            .into_iter()
            .map(|t| t.kind)
            .collect();
        assert_eq!(due, vec!["repo"]);
        assert_eq!(s.knowledge_due(later, 50).unwrap().len(), 3);

        let worker = Reader::default();
        let coord = Reader {
            coordinator_of: Some(a.id.clone()),
            project: None,
        };
        let hits = s.chunks_search("refunds", None, &worker, 10).unwrap();
        let texts: Vec<_> = hits.iter().map(|h| h.text.as_str()).collect();
        assert!(
            texts.contains(&"Refunds within thirty days")
                && texts.contains(&"Bolt refunds take a week")
        );
        assert!(
            !texts.iter().any(|t| t.contains("Invoice")),
            "a worker read coordinator-only email"
        );
        let hits = s
            .chunks_search("refunded invoice", None, &coord, 10)
            .unwrap();
        assert_eq!(hits[0].text, "Invoice 4411 refunded to Dana");
        assert_eq!(hits[0].kind, "email");
        // Another project's coordinator doesn't read it either.
        let b_coord = Reader {
            coordinator_of: Some(b.id.clone()),
            project: None,
        };
        assert!(
            s.chunks_search("invoice", None, &b_coord, 10)
                .unwrap()
                .is_empty()
        );
        // A project filter.
        let only_b = Reader {
            project: Some(b.id.clone()),
            ..Default::default()
        };
        let hits = s.chunks_search("refunds", None, &only_b, 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].resource, "https://bolt.dev");

        // Making the site coordinator-only hides it from workers at once.
        s.resource_save(
            &a.id,
            &ResourceInput {
                id: Some(site.id.clone()),
                kind: "website".into(),
                title: "Acme".into(),
                url: Some("https://acme.dev".into()),
                access: Some("coordinator".into()),
                ..Default::default()
            },
            "user",
            now + chrono::Duration::seconds(1),
        )
        .unwrap();
        let hits = s.chunks_search("refunds", None, &worker, 10).unwrap();
        assert!(hits.iter().all(|h| h.resource_id != site.id));
        assert!(
            s.chunks_search("refunds", None, &coord, 10)
                .unwrap()
                .iter()
                .any(|h| h.resource_id == site.id)
        );

        // Failing keeps what was indexed; last used shows up.
        let edited = s
            .knowledge_due(now, 50)
            .unwrap()
            .into_iter()
            .find(|t| t.id == site.id)
            .unwrap();
        s.knowledge_failed(&edited, "failed", "site down", later)
            .unwrap();
        s.knowledge_mark_used(&[&site.id], now).unwrap();
        let r = s
            .project_resources(&a.id, "coordinator")
            .unwrap()
            .into_iter()
            .find(|r| r.id == site.id)
            .unwrap();
        assert_eq!((r.status.as_str(), r.chunks), ("failed", 2));
        assert_eq!(r.index_error.as_deref(), Some("site down"));
        assert!(r.last_used_at.is_some());

        // Removing a resource forgets its pieces.
        s.resource_delete(&a.id, &site.id).unwrap();
        s.knowledge_due(now, 50).unwrap();
        assert!(
            s.chunks_search("thirty", None, &coord, 10)
                .unwrap()
                .is_empty()
        );
    }
}
