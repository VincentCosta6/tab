//! Routing items to projects as they come in.
//!
//! Most items live in a container upstream (a repo, a Sentry project, a
//! Linear team) that maps to one project, so tab routes containers and the
//! items follow. A connector names an item's container as an entity with
//! `rel = "container"` ([`CONTAINER_REL`]); a project's anchors
//! ([`ProjectAnchor`]) and its repos say which containers are its own.
//!
//! When an item nobody has filed comes in, in order:
//! 1. its container is anchored to one project: it goes there;
//! 2. the graph votes: anchored entities it links to (a PR in an anchored
//!    repo) and items it shares something specific with that are in a
//!    project. One project wins outright and it goes there; a split vote
//!    becomes a suggestion;
//! 3. later, once it has a vector, a project whose items read like it is
//!    suggested ([`Store::suggest_by_similarity`]).
//!
//! tab never moves an item the person filed (or took out of a project).
//! Undoing a route by hand records a `never` anchor, so it isn't repeated.
//! Anchors come from the person ("Always send … here?"), from scanning a
//! project's checkout (`crate::routing`) and from names that match
//! ([`Store::propose_anchors`]); the last two wait as `proposed`.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use super::{Store, parse_ts, ts};
use crate::model::{ANCHOR_STATES, CONTAINER_REL, EntityRef, ProjectAnchor};

/// Entity kinds too broad to say which project an item is in when two
/// items share them (and the project's own node).
const BROAD: &str = "'person', 'repo', 'channel', 'org', 'team', 'mailbox', 'domain', 'project'";

/// An entity on more items than this doesn't vote: it's on everything.
const VOTE_MAX_ITEMS: i64 = 40;

const ANCHOR_COLS: &str =
    "id, project_id, kind, key, label, state, source, reason, created_at, updated_at";

fn anchor_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ProjectAnchor> {
    Ok(ProjectAnchor {
        id: r.get("id")?,
        project_id: r.get("project_id")?,
        kind: r.get("kind")?,
        key: r.get("key")?,
        label: r.get("label")?,
        state: r.get("state")?,
        source: r.get("source")?,
        reason: r.get("reason")?,
        created_at: parse_ts(&r.get::<_, String>("created_at")?)?,
        updated_at: parse_ts(&r.get::<_, String>("updated_at")?)?,
    })
}

/// How a container reads in a reason: "Sentry project tab-desktop",
/// "repo acme/web".
pub fn container_name(kind: &str, label: &str) -> String {
    match kind.split_once('.') {
        Some((service, what)) => {
            let mut s = service.chars();
            let service = s
                .next()
                .map(|c| c.to_uppercase().chain(s).collect::<String>())
                .unwrap_or_default();
            format!("{service} {} {label}", what.replace('_', " "))
        }
        None => format!("{} {label}", kind.replace('_', " ")),
    }
}

/// The projects a container goes to, and the ones it must not: its anchors,
/// plus the projects that list it as a repo.
fn container_projects(
    conn: &Connection,
    kind: &str,
    key: &str,
) -> anyhow::Result<(BTreeSet<String>, BTreeSet<String>)> {
    let (mut always, mut never) = (BTreeSet::new(), BTreeSet::new());
    let mut st = conn.prepare_cached(
        "SELECT project_id, state FROM project_anchors WHERE kind = ?1 AND key = ?2",
    )?;
    for r in st.query_map(params![kind, key], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })? {
        let (p, state) = r?;
        match state.as_str() {
            "always" => {
                always.insert(p);
            }
            "never" => {
                never.insert(p);
            }
            _ => {}
        }
    }
    if kind == "repo"
        && let Some(repo) = key.strip_prefix("github:")
    {
        let mut st = conn.prepare_cached(
            "SELECT project_id FROM project_repos WHERE repo = ?1 COLLATE NOCASE",
        )?;
        for p in st.query_map([repo], |r| r.get::<_, String>(0))? {
            always.insert(p?);
        }
    }
    always.retain(|p| !never.contains(p));
    Ok((always, never))
}

/// An item's containers: `(kind, key, label)`.
fn containers(conn: &Connection, item_id: &str) -> anyhow::Result<Vec<(String, String, String)>> {
    let mut st = conn.prepare_cached(
        "SELECT e.kind, e.key, e.label FROM item_entities ie JOIN entities e ON e.id = ie.entity_id
         WHERE ie.item_id = ?1 AND ie.rel = ?2 ORDER BY e.id",
    )?;
    let out = st
        .query_map(params![item_id, CONTAINER_REL], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?
        .collect::<Result<_, _>>()?;
    Ok(out)
}

/// Puts the item in the project (or takes it out) and gives the graph the
/// matching `in` edge to the project's node.
pub(crate) fn link_project(
    tx: &Transaction<'_>,
    item_id: &str,
    project: Option<&(String, String)>,
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    tx.execute(
        "DELETE FROM item_entities WHERE item_id = ?1
           AND entity_id IN (SELECT id FROM entities WHERE kind = 'project' AND key LIKE 'project:%')",
        [item_id],
    )?;
    if let Some((id, name)) = project {
        let entity: i64 = tx.query_row(
            "INSERT INTO entities (kind, key, label, created_at, updated_at) VALUES ('project', ?1, ?2, ?3, ?3)
             ON CONFLICT (kind, key) DO UPDATE SET label = excluded.label, updated_at = excluded.updated_at
             RETURNING id",
            params![format!("project:{id}"), name, ts(now)],
            |r| r.get(0),
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO item_entities (item_id, entity_id, rel) VALUES (?1, ?2, 'in')",
            params![item_id, entity],
        )?;
    }
    Ok(())
}

fn project_named(tx: &Transaction<'_>, id: &str) -> anyhow::Result<Option<(String, String)>> {
    Ok(tx
        .query_row("SELECT id, name FROM projects WHERE id = ?1", [id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .optional()?)
}

fn route_to(
    tx: &Transaction<'_>,
    item_id: &str,
    project_id: &str,
    reason: &str,
    now: DateTime<Utc>,
) -> anyhow::Result<bool> {
    let Some(project) = project_named(tx, project_id)? else {
        return Ok(false);
    };
    tx.execute(
        "UPDATE items SET project_id = ?2, project_by = 'auto', routed_by = ?3,
             suggested_project_id = NULL, suggested_by = NULL
         WHERE id = ?1",
        params![item_id, project_id, reason],
    )?;
    link_project(tx, item_id, Some(&project), now)?;
    Ok(true)
}

fn suggest(
    tx: &Transaction<'_>,
    item_id: &str,
    project_id: &str,
    reason: &str,
) -> anyhow::Result<()> {
    tx.execute(
        "UPDATE items SET suggested_project_id = ?2, suggested_by = ?3 WHERE id = ?1
           AND (suggested_project_id IS NOT ?2 OR suggested_by IS NOT ?3)",
        params![item_id, project_id, reason],
    )?;
    Ok(())
}

/// Routes an item nobody has filed (see the module docs). Run as it's
/// stored.
pub(crate) fn route_item(
    tx: &Transaction<'_>,
    item_id: &str,
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    let filed: Option<(Option<String>, Option<String>)> = tx
        .query_row(
            "SELECT project_id, project_by FROM items WHERE id = ?1",
            [item_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((None, None)) = filed else {
        return Ok(());
    };

    // 1. The container.
    let mine = containers(tx, item_id)?;
    let mut never = BTreeSet::new();
    let mut by_container: BTreeMap<String, String> = BTreeMap::new();
    for (kind, key, label) in &mine {
        let (always, no) = container_projects(tx, kind, key)?;
        never.extend(no);
        for p in always {
            by_container
                .entry(p)
                .or_insert_with(|| container_name(kind, label));
        }
    }
    by_container.retain(|p, _| !never.contains(p));
    if by_container.len() == 1 {
        let (p, reason) = by_container.into_iter().next().expect("one");
        route_to(tx, item_id, &p, &reason, now)?;
        return Ok(());
    }

    // 2. The graph. Anchored entities that aren't the container (the repo of
    // a PR the item links to)...
    let mut votes: BTreeMap<String, (usize, String)> = BTreeMap::new();
    let linked: Vec<(String, String, String)> = tx
        .prepare_cached(
            "SELECT e.kind, e.key, e.label FROM item_entities ie JOIN entities e ON e.id = ie.entity_id
             WHERE ie.item_id = ?1 AND ie.rel != ?2",
        )?
        .query_map(params![item_id, CONTAINER_REL], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?
        .collect::<Result<_, _>>()?;
    for (kind, key, label) in &linked {
        let (always, no) = container_projects(tx, kind, key)?;
        never.extend(no);
        for p in always {
            let v = votes
                .entry(p)
                .or_insert((0, format!("linked to {}", container_name(kind, label))));
            v.0 += 1;
        }
    }
    // ...and specific things it shares with items in a project (the same
    // PR, ticket or thread).
    let shared: Vec<(String, String)> = tx
        .prepare_cached(&format!(
            "SELECT i.project_id, e.label FROM item_entities mine
             JOIN entities e ON e.id = mine.entity_id
             JOIN item_entities other ON other.entity_id = mine.entity_id AND other.item_id != mine.item_id
             JOIN items i ON i.id = other.item_id
             WHERE mine.item_id = ?1 AND mine.rel != ?2 AND i.project_id IS NOT NULL
               AND e.kind NOT IN ({BROAD})
               AND (SELECT COUNT(*) FROM item_entities x WHERE x.entity_id = e.id) <= ?3"
        ))?
        .query_map(params![item_id, CONTAINER_REL, VOTE_MAX_ITEMS], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?
        .collect::<Result<_, _>>()?;
    for (p, label) in shared {
        let v = votes
            .entry(p)
            .or_insert((0, format!("shares {label} with items there")));
        v.0 += 1;
    }
    votes.retain(|p, _| !never.contains(p));
    // A container tied to several projects is a split vote too.
    for (p, reason) in by_container {
        votes.entry(p).or_insert((1, reason));
    }
    match votes.len() {
        0 => {}
        1 => {
            let (p, (_, reason)) = votes.into_iter().next().expect("one");
            route_to(tx, item_id, &p, &reason, now)?;
        }
        _ => {
            let (p, (_, reason)) = votes
                .into_iter()
                .max_by(|a, b| a.1.0.cmp(&b.1.0).then_with(|| b.0.cmp(&a.0)))
                .expect("some");
            suggest(tx, item_id, &p, &reason)?;
        }
    }
    Ok(())
}

/// Lowercase words of a name without the suffixes that tell apps of one
/// product apart, run together: `tab-desktop` and `Tab` are both `tab`.
fn name_core(name: &str) -> String {
    const SUFFIXES: &[&str] = &[
        "app",
        "web",
        "desktop",
        "api",
        "server",
        "backend",
        "frontend",
        "ios",
        "android",
        "mobile",
        "site",
        "www",
        "prod",
        "production",
        "staging",
        "dev",
        "js",
        "rs",
    ];
    let words: Vec<String> = name
        .to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect();
    let kept: Vec<&String> = words
        .iter()
        .filter(|w| !SUFFIXES.contains(&w.as_str()))
        .collect();
    // A name that's only a suffix ("web") is itself.
    if kept.is_empty() {
        words.concat()
    } else {
        kept.into_iter().map(String::as_str).collect()
    }
}

/// Whether two names are the same thing, loosely (see [`name_core`]).
pub fn names_match(a: &str, b: &str) -> bool {
    let (a, b) = (name_core(a), name_core(b));
    a.len() >= 3 && a == b
}

/// The short name of a container: its label, and the last part of its key.
fn container_names(key: &str, label: &str) -> Vec<String> {
    let tail = key.rsplit([':', '/']).next().unwrap_or(key);
    let mut v = vec![label.to_string()];
    if !tail.eq_ignore_ascii_case(label) {
        v.push(tail.to_string());
    }
    v
}

/// The service a container belongs to: `sentry` for `sentry.project`,
/// `github` for a repo.
fn service_of(kind: &str) -> &str {
    match kind {
        "repo" => "github",
        k => k.split('.').next().unwrap_or(k),
    }
}

/// Something a scan of a project's checkout found (see `crate::routing`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct RepoHint {
    /// `sentry` (a project slug, or `org/slug`), `sentry_dsn` (a project's
    /// numeric id), `linear` (a team key), or where an app's name came
    /// from: `npm`, `cloudflare`, `vercel`, `fly`.
    pub service: String,
    pub value: String,
    /// Where, relative to the checkout.
    pub file: String,
}

impl RepoHint {
    fn reason(&self) -> String {
        match self.service.as_str() {
            "sentry" => format!("Sentry project {} in {}", self.value, self.file),
            "sentry_dsn" => format!("Sentry DSN in {}", self.file),
            "linear" => format!("Linear keys like {}-123 in {}", self.value, self.file),
            _ => format!("{} in {}", self.value, self.file),
        }
    }
}

/// Whether a hint names a container.
fn hint_matches(
    conn: &Connection,
    h: &RepoHint,
    kind: &str,
    key: &str,
    label: &str,
) -> anyhow::Result<bool> {
    let service = service_of(kind);
    Ok(match h.service.as_str() {
        "sentry" => {
            service == "sentry"
                && (key.eq_ignore_ascii_case(&format!("sentry:{}", h.value))
                    || key.to_lowercase().ends_with(&format!("/{}", h.value.to_lowercase())))
        }
        "linear" => service == "linear" && key.eq_ignore_ascii_case(&format!("linear:team:{}", h.value)),
        // The DSN names the project by number: look for an item that has
        // both that number and the container.
        "sentry_dsn" => {
            service == "sentry"
                && conn
                    .prepare_cached(
                        "SELECT EXISTS (SELECT 1 FROM item_entities a JOIN entities ea ON ea.id = a.entity_id
                           JOIN item_entities b ON b.item_id = a.item_id JOIN entities eb ON eb.id = b.entity_id
                           WHERE ea.kind = 'sentry.project_id' AND ea.key = ?1 AND eb.kind = ?2 AND eb.key = ?3)",
                    )?
                    .query_row(params![format!("sentry:{}", h.value), kind, key], |r| r.get(0))?
        }
        _ => container_names(key, label).iter().any(|n| names_match(n, &h.value)),
    })
}

impl Store {
    /// A project's anchors: `always` first, then proposals, then `never`.
    pub fn project_anchors(&self, project_id: &str) -> anyhow::Result<Vec<ProjectAnchor>> {
        let conn = self.conn();
        let out = conn
            .prepare(&format!(
                "SELECT {ANCHOR_COLS} FROM project_anchors WHERE project_id = ?1
                 ORDER BY CASE state WHEN 'always' THEN 0 WHEN 'proposed' THEN 1 ELSE 2 END, label COLLATE NOCASE"
            ))?
            .query_map([project_id], anchor_row)?
            .collect::<Result<_, _>>()?;
        Ok(out)
    }

    /// Ties a container to a project, or changes how. A proposal never
    /// replaces what the person decided. An `always` anchor routes the
    /// container's unfiled items straight away; returns the anchor and how
    /// many it routed.
    #[allow(clippy::too_many_arguments)]
    pub fn anchor_save(
        &self,
        project_id: &str,
        container: &EntityRef,
        state: &str,
        source: &str,
        reason: &str,
        now: DateTime<Utc>,
    ) -> anyhow::Result<(ProjectAnchor, usize)> {
        anyhow::ensure!(
            ANCHOR_STATES.contains(&state),
            "unknown anchor state {state:?}"
        );
        anyhow::ensure!(
            !container.kind.is_empty() && !container.key.is_empty(),
            "an anchor needs a container"
        );
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        anyhow::ensure!(
            project_named(&tx, project_id)?.is_some(),
            "no project {project_id}"
        );
        let anchor = save_anchor(&tx, project_id, container, state, source, reason, now)?;
        let routed = if anchor.state == "always" {
            route_container(&tx, &container.kind, &container.key, now)?
        } else {
            0
        };
        tx.commit()?;
        Ok((anchor, routed))
    }

    /// Removes an anchor. Returns its project.
    pub fn anchor_delete(&self, id: &str) -> anyhow::Result<Option<String>> {
        Ok(self
            .conn()
            .query_row(
                "DELETE FROM project_anchors WHERE id = ?1 RETURNING project_id",
                [id],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// After the person files an item in its project: the container worth
    /// offering to always send there, if it isn't already.
    pub fn route_offer(&self, item_id: &str) -> anyhow::Result<Option<EntityRef>> {
        let conn = self.conn();
        let Some(project) = conn
            .query_row(
                "SELECT project_id FROM items WHERE id = ?1",
                [item_id],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten()
        else {
            return Ok(None);
        };
        for (kind, key, label) in containers(&conn, item_id)? {
            let (always, _) = container_projects(&conn, &kind, &key)?;
            if !always.contains(&project) {
                return Ok(Some(EntityRef::new(&kind, key, label, CONTAINER_REL)));
            }
        }
        Ok(None)
    }

    /// Suggests a project for an item that has none and that nobody has
    /// filed. Returns whether it changed.
    pub fn item_suggest(
        &self,
        item_id: &str,
        project_id: &str,
        reason: &str,
    ) -> anyhow::Result<bool> {
        Ok(self.conn().execute(
            "UPDATE items SET suggested_project_id = ?2, suggested_by = ?3
             WHERE id = ?1 AND project_id IS NULL AND project_by IS NULL
               AND suggested_project_id IS NOT ?2",
            params![item_id, project_id, reason],
        )? > 0)
    }

    /// Suggests projects by meaning for items that have a vector but no
    /// project or suggestion: the project whose items' centroid is nearest,
    /// when it's near enough and clearly nearer than the next. Returns the
    /// items it suggested a project for.
    pub fn suggest_by_similarity(&self, item_ids: &[String]) -> anyhow::Result<Vec<String>> {
        /// Projects with fewer filed items than this have no centroid yet.
        const MIN_ITEMS: usize = 3;
        const MIN_SIMILARITY: f32 = 0.55;
        const MIN_MARGIN: f32 = 0.05;
        if item_ids.is_empty() || !self.has_vectors()? {
            return Ok(vec![]);
        }
        let conn = self.conn();
        let vector = |doc: &str| -> anyhow::Result<Option<Vec<f32>>> {
            Ok(conn
                .prepare_cached("SELECT embedding FROM doc_vec WHERE doc = ?1")?
                .query_row([doc], |r| r.get::<_, Vec<u8>>(0))
                .optional()?
                .map(|b| {
                    b.as_chunks::<4>()
                        .0
                        .iter()
                        .map(|c| f32::from_le_bytes(*c))
                        .collect()
                }))
        };
        let mut todo = vec![];
        for id in item_ids {
            let open: bool = conn
                .query_row(
                    "SELECT project_id IS NULL AND project_by IS NULL AND suggested_project_id IS NULL
                     FROM items WHERE id = ?1",
                    [id],
                    |r| r.get(0),
                )
                .optional()?
                .unwrap_or(false);
            if open && let Some(v) = vector(&format!("item:{id}"))? {
                todo.push((id.clone(), v));
            }
        }
        if todo.is_empty() {
            return Ok(vec![]);
        }
        // Each project's centroid: its recent items and its notes.
        let projects: Vec<(String, String)> = conn
            .prepare("SELECT id, name FROM projects")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        let mut centroids = vec![];
        for (pid, name) in projects {
            let docs: Vec<String> = conn
                .prepare_cached(
                    "SELECT 'item:' || id FROM items WHERE project_id = ?1
                     ORDER BY occurred_at DESC LIMIT 100",
                )?
                .query_map([&pid], |r| r.get(0))?
                .chain(
                    conn.prepare_cached(
                        "SELECT 'note:' || id FROM project_notes WHERE project_id = ?1",
                    )?
                    .query_map([&pid], |r| r.get(0))?,
                )
                .collect::<Result<_, _>>()?;
            let vecs: Vec<Vec<f32>> = docs
                .iter()
                .filter_map(|d| vector(d).transpose())
                .collect::<anyhow::Result<_>>()?;
            if vecs.len() < MIN_ITEMS {
                continue;
            }
            let dim = vecs[0].len();
            let mut c = vec![0f32; dim];
            for v in vecs.iter().filter(|v| v.len() == dim) {
                for (a, b) in c.iter_mut().zip(v) {
                    *a += b;
                }
            }
            centroids.push((pid, name, c));
        }
        drop(conn);
        let mut out = vec![];
        for (id, v) in todo {
            let mut scored: Vec<(f32, &str, &str)> = centroids
                .iter()
                .map(|(p, n, c)| (cosine(&v, c), p.as_str(), n.as_str()))
                .collect();
            scored.sort_by(|a, b| b.0.total_cmp(&a.0));
            let Some(&(best, pid, name)) = scored.first() else {
                continue;
            };
            let second = scored.get(1).map_or(0.0, |s| s.0);
            if best >= MIN_SIMILARITY && best - second >= MIN_MARGIN {
                let reason = format!("reads like {name}'s items");
                if self.item_suggest(&id, pid, &reason)? {
                    out.push(id);
                }
            }
        }
        Ok(out)
    }

    /// Replaces what a scan of a project's checkouts found.
    pub fn repo_hints_replace(&self, project_id: &str, hints: &[RepoHint]) -> anyhow::Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM repo_hints WHERE project_id = ?1", [project_id])?;
        for h in hints {
            tx.execute(
                "INSERT OR IGNORE INTO repo_hints (project_id, service, value, file) VALUES (?1, ?2, ?3, ?4)",
                params![project_id, h.service, h.value, h.file],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Proposes anchors for containers that have none: from what scanning
    /// the projects' checkouts found, else from a container named like one
    /// project or its repo. Only one project per container; the person
    /// confirms. Returns the new proposals.
    pub fn propose_anchors(&self, now: DateTime<Utc>) -> anyhow::Result<Vec<ProjectAnchor>> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let hints: Vec<(String, RepoHint)> = tx
            .prepare("SELECT project_id, service, value, file FROM repo_hints ORDER BY project_id, service, value")?
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    RepoHint { service: r.get(1)?, value: r.get(2)?, file: r.get(3)? },
                ))
            })?
            .collect::<Result<_, _>>()?;
        let names: Vec<(String, String, String)> = tx
            .prepare(
                "SELECT id, name, 'project ' || name FROM projects
                 UNION ALL SELECT project_id, repo, 'repo ' || repo FROM project_repos",
            )?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<_, _>>()?;
        let mut found: Vec<(String, EntityRef, &'static str, String)> = vec![];
        // A Sentry project named in full needs no items to be proposed.
        for (p, h) in &hints {
            if h.service == "sentry" && h.value.contains('/') {
                let slug = h.value.rsplit('/').next().unwrap_or(&h.value);
                found.push((
                    p.clone(),
                    EntityRef::new(
                        "sentry.project",
                        format!("sentry:{}", h.value),
                        slug,
                        CONTAINER_REL,
                    ),
                    "scan",
                    h.reason(),
                ));
            }
        }
        let open: Vec<(String, String, String)> = tx
            .prepare(
                "SELECT DISTINCT e.kind, e.key, e.label FROM entities e
                 JOIN item_entities ie ON ie.entity_id = e.id AND ie.rel = ?1
                 WHERE NOT EXISTS (SELECT 1 FROM project_anchors a WHERE a.kind = e.kind AND a.key = e.key)
                 ORDER BY e.id",
            )?
            .query_map([CONTAINER_REL], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<_, _>>()?;
        for (kind, key, label) in open {
            if !container_projects(&tx, &kind, &key)?.0.is_empty() {
                continue;
            }
            let mut by_hint: BTreeMap<&str, String> = BTreeMap::new();
            for (p, h) in &hints {
                if hint_matches(&tx, h, &kind, &key, &label)? {
                    by_hint.entry(p.as_str()).or_insert_with(|| h.reason());
                }
            }
            let by_name: BTreeMap<&str, String> = names
                .iter()
                .filter(|(_, n, _)| {
                    let n = n.rsplit('/').next().unwrap_or(n);
                    container_names(&key, &label)
                        .iter()
                        .any(|c| names_match(c, n))
                })
                .map(|(p, _, what)| (p.as_str(), format!("named like {what}")))
                .collect();
            let (picked, source) = if by_hint.is_empty() {
                (by_name, "match")
            } else {
                (by_hint, "scan")
            };
            if picked.len() == 1 {
                let (p, reason) = picked.into_iter().next().expect("one");
                found.push((
                    p.to_string(),
                    EntityRef::new(&kind, &key, &label, CONTAINER_REL),
                    source,
                    reason,
                ));
            }
        }
        let mut out = vec![];
        for (p, c, source, reason) in found {
            let exists: bool = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM project_anchors WHERE kind = ?1 AND key = ?2)",
                params![c.kind, c.key],
                |r| r.get(0),
            )?;
            if !exists {
                out.push(save_anchor(&tx, &p, &c, "proposed", source, &reason, now)?);
            }
        }
        tx.commit()?;
        Ok(out)
    }

    /// Whether the vector table exists yet.
    fn has_vectors(&self) -> anyhow::Result<bool> {
        Ok(self.conn().query_row(
            "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE name = 'doc_vec')",
            [],
            |r| r.get(0),
        )?)
    }
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    let (mut dot, mut na, mut nb) = (0f32, 0f32, 0f32);
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na.sqrt() * nb.sqrt())
    }
}

fn save_anchor(
    tx: &Transaction<'_>,
    project_id: &str,
    c: &EntityRef,
    state: &str,
    source: &str,
    reason: &str,
    now: DateTime<Utc>,
) -> anyhow::Result<ProjectAnchor> {
    // A proposal doesn't overwrite a decision.
    tx.execute(
        "INSERT INTO project_anchors (id, project_id, kind, key, label, state, source, reason, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)
         ON CONFLICT (project_id, kind, key) DO UPDATE SET label = excluded.label, state = excluded.state,
             source = excluded.source, reason = excluded.reason, updated_at = excluded.updated_at
         WHERE excluded.state != 'proposed' OR project_anchors.state = 'proposed'",
        params![
            uuid::Uuid::new_v4().to_string(),
            project_id,
            c.kind,
            c.key,
            c.label,
            state,
            source,
            reason,
            ts(now)
        ],
    )?;
    Ok(tx.query_row(
        &format!("SELECT {ANCHOR_COLS} FROM project_anchors WHERE project_id = ?1 AND kind = ?2 AND key = ?3"),
        params![project_id, c.kind, c.key],
        anchor_row,
    )?)
}

/// Routes the unfiled items in a container. Returns how many moved.
fn route_container(
    tx: &Transaction<'_>,
    kind: &str,
    key: &str,
    now: DateTime<Utc>,
) -> anyhow::Result<usize> {
    let ids: Vec<String> = tx
        .prepare(
            "SELECT i.id FROM items i JOIN item_entities ie ON ie.item_id = i.id AND ie.rel = ?3
             JOIN entities e ON e.id = ie.entity_id
             WHERE e.kind = ?1 AND e.key = ?2 AND i.project_id IS NULL AND i.project_by IS NULL",
        )?
        .query_map(params![kind, key, CONTAINER_REL], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    let mut n = 0;
    for id in ids {
        route_item(tx, &id, now)?;
        let filed: bool = tx.query_row(
            "SELECT project_id IS NOT NULL FROM items WHERE id = ?1",
            [&id],
            |r| r.get(0),
        )?;
        n += filed as usize;
    }
    Ok(n)
}

/// The person (or an agent for them) filed an item, or took it out of its
/// project. Undoing tab's route records `never` for the containers that
/// routed it, so the same container isn't sent there again.
pub(crate) fn filed_by_person(
    tx: &Transaction<'_>,
    item_id: &str,
    project_id: Option<&str>,
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    let before: Option<(Option<String>, Option<String>)> = tx
        .query_row(
            "SELECT project_id, project_by FROM items WHERE id = ?1",
            [item_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if let Some((Some(old), Some(by))) = &before
        && by == "auto"
        && Some(old.as_str()) != project_id
    {
        for (kind, key, label) in containers(tx, item_id)? {
            if container_projects(tx, &kind, &key)?.0.contains(old) {
                let c = EntityRef::new(&kind, key, label, CONTAINER_REL);
                save_anchor(tx, old, &c, "never", "undo", "", now)?;
            }
        }
    }
    tx.execute(
        "UPDATE items SET project_by = 'person', routed_by = NULL, suggested_project_id = NULL,
             suggested_by = NULL
         WHERE id = ?1",
        [item_id],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::IncomingEvent;
    use crate::store::Upsert;

    fn sentry(project: &str) -> EntityRef {
        EntityRef::new(
            "sentry.project",
            format!("sentry:acme/{project}"),
            project,
            CONTAINER_REL,
        )
    }

    fn add(s: &Store, id: &str, entities: Vec<EntityRef>) -> QueueItem {
        let e = IncomingEvent {
            source: "test".into(),
            external_id: id.into(),
            kind: "sentry.issue".into(),
            title: id.into(),
            occurred_at: Utc::now(),
            entities,
            ..Default::default()
        };
        match s.upsert(&e, Utc::now()).unwrap() {
            Upsert::Inserted(i) | Upsert::Updated(i) => i,
            Upsert::Unchanged => panic!("unchanged"),
        }
    }

    fn project(s: &Store, name: &str) -> String {
        s.project_save(None, name, "#7c5cff", "folder", Utc::now())
            .unwrap()
            .id
    }

    use crate::model::QueueItem;

    #[test]
    fn anchored_containers_route_at_ingest() {
        let s = Store::in_memory().unwrap();
        let now = Utc::now();
        let tab = project(&s, "tab");
        s.anchor_save(&tab, &sentry("tab-desktop"), "always", "person", "", now)
            .unwrap();

        let i = add(&s, "1", vec![sentry("tab-desktop")]);
        assert_eq!(i.project_id.as_deref(), Some(tab.as_str()));
        assert_eq!(i.routed_by.as_deref(), Some("Sentry project tab-desktop"));
        assert_eq!(s.project_items(&tab, None, 5).unwrap().len(), 1);

        // Another container stays put.
        assert!(add(&s, "2", vec![sentry("billing")]).project_id.is_none());

        // A repo listed on the project is an anchor too.
        s.repo_save(
            &tab,
            "https://github.com/acme/tab.git",
            None,
            None,
            "user",
            now,
        )
        .unwrap();
        let pr = add(
            &s,
            "3",
            vec![EntityRef::new(
                "repo",
                "github:acme/tab",
                "acme/tab",
                CONTAINER_REL,
            )],
        );
        assert_eq!(pr.project_id.as_deref(), Some(tab.as_str()));
        assert_eq!(pr.routed_by.as_deref(), Some("repo acme/tab"));
    }

    #[test]
    fn never_overrides_the_person() {
        let s = Store::in_memory().unwrap();
        let now = Utc::now();
        let (tab, other) = (project(&s, "tab"), project(&s, "other"));
        let i = add(&s, "1", vec![sentry("web")]);
        s.item_set_project(&i.id, Some(&other), now).unwrap();
        // Anchoring later, or new activity, doesn't move it.
        s.anchor_save(&tab, &sentry("web"), "always", "person", "", now)
            .unwrap();
        let mut again = add(&s, "1", vec![sentry("web")]);
        again = s.get(&again.id).unwrap().unwrap();
        assert_eq!(again.project_id.as_deref(), Some(other.as_str()));
        assert!(again.routed_by.is_none());

        // Taken out of a project by hand: stays out.
        let j = add(&s, "2", vec![]);
        s.item_set_project(&j.id, None, now).unwrap();
        let mut e = s.get(&j.id).unwrap().unwrap().event;
        e.entities = vec![sentry("web")];
        e.title = "new activity".into();
        s.upsert(&e, now).unwrap();
        assert!(s.get(&j.id).unwrap().unwrap().project_id.is_none());
    }

    #[test]
    fn an_always_anchor_routes_what_is_waiting_and_undo_remembers() {
        let s = Store::in_memory().unwrap();
        let now = Utc::now();
        let tab = project(&s, "tab");
        let a = add(&s, "1", vec![sentry("web")]);
        let b = add(&s, "2", vec![sentry("web")]);
        // The person files one; tab offers the container.
        s.item_set_project(&a.id, Some(&tab), now).unwrap();
        let offer = s.route_offer(&a.id).unwrap().unwrap();
        assert_eq!(offer.key, "sentry:acme/web");
        let (anchor, routed) = s
            .anchor_save(&tab, &offer, "always", "learned", "", now)
            .unwrap();
        assert_eq!((anchor.state.as_str(), routed), ("always", 1));
        assert!(s.route_offer(&a.id).unwrap().is_none(), "already anchored");
        assert_eq!(
            s.get(&b.id).unwrap().unwrap().project_id.as_deref(),
            Some(tab.as_str())
        );

        // Undoing tab's route turns the anchor into a `never`...
        s.item_set_project(&b.id, None, now).unwrap();
        let anchors = s.project_anchors(&tab).unwrap();
        assert_eq!(
            (anchors[0].state.as_str(), anchors[0].source.as_str()),
            ("never", "undo")
        );
        // ...so the next one isn't routed there, and isn't proposed again.
        assert!(add(&s, "3", vec![sentry("web")]).project_id.is_none());
        assert!(s.propose_anchors(now).unwrap().is_empty());
        // A proposal never overwrites a decision.
        s.anchor_save(&tab, &sentry("web"), "proposed", "match", "", now)
            .unwrap();
        assert_eq!(s.project_anchors(&tab).unwrap()[0].state, "never");
    }

    #[test]
    fn the_graph_votes() {
        let s = Store::in_memory().unwrap();
        let now = Utc::now();
        let (tab, infra) = (project(&s, "tab"), project(&s, "infra"));
        s.repo_save(&tab, "acme/tab", None, None, "user", now)
            .unwrap();
        // A Sentry issue that links to a PR in an anchored repo.
        let linked = add(
            &s,
            "1",
            vec![
                sentry("unknown"),
                EntityRef::new("repo", "github:acme/tab", "acme/tab", "in"),
                EntityRef::new("pull_request", "github:acme/tab#7", "acme/tab#7", "about"),
            ],
        );
        assert_eq!(linked.project_id.as_deref(), Some(tab.as_str()));
        assert_eq!(linked.routed_by.as_deref(), Some("linked to repo acme/tab"));

        // Sharing a specific thing with an item in a project.
        let ticket = || EntityRef::new("linear.issue", "linear:INF-3", "INF-3", "about");
        let filed = add(&s, "2", vec![ticket()]);
        s.item_set_project(&filed.id, Some(&infra), now).unwrap();
        let follows = add(&s, "3", vec![ticket()]);
        assert_eq!(follows.project_id.as_deref(), Some(infra.as_str()));

        // Votes for two projects: only a suggestion.
        let split = add(
            &s,
            "4",
            vec![
                ticket(),
                EntityRef::new("pull_request", "github:acme/tab#7", "acme/tab#7", "about"),
            ],
        );
        assert!(split.project_id.is_none());
        assert!(split.suggestion.is_some());
        // Accepting it is filing by hand.
        let p = split.suggestion.unwrap().project_id;
        let moved = s
            .item_set_project(&split.id, Some(&p), now)
            .unwrap()
            .unwrap();
        assert!(moved.suggestion.is_none() && moved.routed_by.is_none());
    }

    #[test]
    fn proposes_anchors_from_hints_and_names() {
        let s = Store::in_memory().unwrap();
        let now = Utc::now();
        let (tab, pay) = (project(&s, "tab"), project(&s, "Payments"));
        s.repo_save(&pay, "acme/checkout", None, None, "user", now)
            .unwrap();
        add(&s, "1", vec![sentry("tab-desktop")]);
        add(&s, "2", vec![sentry("checkout-api")]);
        add(&s, "3", vec![sentry("misc")]);
        add(
            &s,
            "4",
            vec![
                sentry("numbered"),
                EntityRef::new("sentry.project_id", "sentry:4508", "numbered", "about"),
            ],
        );
        add(
            &s,
            "5",
            vec![EntityRef::new(
                "linear.team",
                "linear:team:TAB",
                "Tab",
                CONTAINER_REL,
            )],
        );
        s.repo_hints_replace(
            &tab,
            &[
                RepoHint {
                    service: "sentry_dsn".into(),
                    value: "4508".into(),
                    file: ".env".into(),
                },
                RepoHint {
                    service: "linear".into(),
                    value: "TAB".into(),
                    file: "branches and commits".into(),
                },
                RepoHint {
                    service: "sentry".into(),
                    value: "acme/tab-site".into(),
                    file: ".env".into(),
                },
            ],
        )
        .unwrap();
        let got: BTreeSet<(String, String, String)> = s
            .propose_anchors(now)
            .unwrap()
            .into_iter()
            .map(|a| (a.project_id, a.key, a.source))
            .collect();
        let want: BTreeSet<(String, String, String)> = [
            (&tab, "sentry:acme/tab-desktop", "match"),
            (&pay, "sentry:acme/checkout-api", "match"),
            (&tab, "sentry:acme/numbered", "scan"),
            (&tab, "linear:team:TAB", "scan"),
            (&tab, "sentry:acme/tab-site", "scan"),
        ]
        .into_iter()
        .map(|(p, k, src)| (p.clone(), k.to_string(), src.to_string()))
        .collect();
        assert_eq!(got, want);
        // Proposals don't route; confirming one does.
        assert!(s.project_items(&tab, None, 10).unwrap().is_empty());
        let a = s
            .project_anchors(&tab)
            .unwrap()
            .into_iter()
            .find(|a| a.key == "sentry:acme/tab-desktop")
            .unwrap();
        let c = EntityRef::new(&a.kind, &a.key, &a.label, CONTAINER_REL);
        assert_eq!(
            s.anchor_save(&tab, &c, "always", "person", "", now)
                .unwrap()
                .1,
            1
        );
        // Nothing new the second time.
        assert!(s.propose_anchors(now).unwrap().is_empty());
    }

    #[test]
    fn names_match_loosely() {
        assert!(names_match("tab-desktop", "tab"));
        assert!(names_match("Payments", "payments-api"));
        assert!(!names_match("tab", "tabby"));
        assert!(!names_match("web", "api"), "{}", name_core("web"));
        assert_eq!(container_name("linear.team", "Core"), "Linear team Core");
    }
}
