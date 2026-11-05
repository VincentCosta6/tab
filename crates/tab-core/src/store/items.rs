use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};

use super::{Store, fts_phrase, parse_ts, ts};
use crate::model::{IncomingEvent, ItemState, QueueItem, Ranking, Urgency};

#[derive(Debug, Clone, PartialEq)]
pub enum Upsert {
    /// First time we've seen this thing.
    Inserted(QueueItem),
    /// New activity on something we already had. It is reopened and needs re-ranking.
    Updated(QueueItem),
    /// Same content as last poll; nothing to do.
    Unchanged,
}

/// Content that, when changed, means "new activity". Timestamps are included
/// so a new comment with identical text still counts.
pub(crate) fn fingerprint(e: &IncomingEvent) -> String {
    format!(
        "{}\x1f{}\x1f{}\x1f{}",
        e.kind,
        e.title,
        e.body.as_deref().unwrap_or(""),
        ts(e.occurred_at)
    )
}

const COLUMNS: &str = "items.id, items.source, items.external_id, items.kind, items.title, items.body, items.url, \
    items.actor, items.context, items.occurred_at, items.blocks, items.entities, items.metadata, items.state, \
    items.snoozed_until, items.score, items.base_score, items.ai_adjustment, items.urgency, items.reason, \
    items.ranked_by, items.created_at, items.updated_at, items.project_id";

/// Display order. Fully deterministic: ties fall back to recency, then id.
/// Narrows a search: when things happened, and which project they're in.
#[derive(Debug, Clone, Default)]
pub struct Scope {
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    pub project_id: Option<String>,
}

impl Scope {
    pub fn contains(&self, i: &QueueItem) -> bool {
        let t = i.event.occurred_at;
        self.since.is_none_or(|s| t >= s)
            && self.until.is_none_or(|u| t < u)
            && self
                .project_id
                .as_ref()
                .is_none_or(|p| i.project_id.as_ref() == Some(p))
    }
}

const ORDER: &str = "ORDER BY COALESCE(items.score, 50) DESC, items.occurred_at DESC, items.id";

fn json_col<T: serde::de::DeserializeOwned + Default>(
    r: &Row<'_>,
    col: &str,
) -> rusqlite::Result<T> {
    let s: String = r.get(col)?;
    Ok(serde_json::from_str(&s).unwrap_or_default())
}

fn row_to_item(r: &Row<'_>) -> rusqlite::Result<QueueItem> {
    let snoozed: Option<String> = r.get("snoozed_until")?;
    let score: Option<i64> = r.get("score")?;
    let urgency: Option<String> = r.get("urgency")?;
    let ranking = match (score, urgency.as_deref().and_then(Urgency::parse)) {
        (Some(score), Some(urgency)) => Some(Ranking {
            score: score.clamp(0, 100) as u8,
            base_score: r
                .get::<_, Option<i64>>("base_score")?
                .unwrap_or(score)
                .clamp(0, 100) as u8,
            ai_adjustment: r.get::<_, Option<i64>>("ai_adjustment")?.map(|a| a as i16),
            urgency,
            reason: r.get::<_, Option<String>>("reason")?.unwrap_or_default(),
            ranked_by: r.get::<_, Option<String>>("ranked_by")?.unwrap_or_default(),
        }),
        _ => None,
    };
    Ok(QueueItem {
        id: r.get("id")?,
        event: IncomingEvent {
            source: r.get("source")?,
            external_id: r.get("external_id")?,
            kind: r.get("kind")?,
            title: r.get("title")?,
            body: r.get("body")?,
            url: r.get("url")?,
            actor: r.get("actor")?,
            context: r.get("context")?,
            occurred_at: parse_ts(&r.get::<_, String>("occurred_at")?)?,
            blocks: json_col(r, "blocks")?,
            entities: json_col(r, "entities")?,
            metadata: json_col(r, "metadata")?,
        },
        state: ItemState::parse(&r.get::<_, String>("state")?),
        ranking,
        snoozed_until: snoozed.as_deref().map(parse_ts).transpose()?,
        project_id: r.get("project_id")?,
        created_at: parse_ts(&r.get::<_, String>("created_at")?)?,
        updated_at: parse_ts(&r.get::<_, String>("updated_at")?)?,
    })
}

pub(crate) fn get_item(conn: &Connection, id: &str) -> anyhow::Result<Option<QueueItem>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM items WHERE id = ?1"),
            [id],
            row_to_item,
        )
        .optional()?)
}

/// See [`Store::item_by_url`]. Query and fragment are ignored.
pub(crate) fn item_by_url(conn: &Connection, url: &str) -> anyhow::Result<Option<QueueItem>> {
    let url = url
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .trim_end_matches('/');
    if !url.starts_with("http") {
        return Ok(None);
    }
    Ok(conn
        .query_row(
                &format!(
                "SELECT {COLUMNS} FROM items
                 WHERE substr(url, 1, length(?1)) = ?1
                   AND (length(url) = length(?1) OR substr(url, length(?1) + 1, 1) IN ('/', '#', '?'))
                 ORDER BY (state = 'done'), occurred_at DESC LIMIT 1"
            ),
            [url],
            row_to_item,
        )
        .optional()?)
}

fn state_list(states: &[ItemState]) -> String {
    // Values come from a closed enum, so inlining them is safe.
    states
        .iter()
        .map(|s| format!("'{}'", s.as_str()))
        .collect::<Vec<_>>()
        .join(",")
}

impl Store {
    pub fn upsert(&self, e: &IncomingEvent, now: DateTime<Utc>) -> anyhow::Result<Upsert> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let fp = fingerprint(e);
        let existing: Option<(String, String)> = tx
            .query_row(
                "SELECT id, fingerprint FROM items WHERE source = ?1 AND external_id = ?2",
                params![e.source, e.external_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let blocks = serde_json::to_string(&e.blocks)?;
        let entities = serde_json::to_string(&e.entities)?;
        let metadata = serde_json::to_string(&e.metadata)?;

        let (id, inserted) = match existing {
            Some((_, old)) if old == fp => return Ok(Upsert::Unchanged),
            Some((id, _)) => {
                tx.execute(
                    "UPDATE items SET kind = ?2, title = ?3, body = ?4, url = ?5, actor = ?6, context = ?7,
                         occurred_at = ?8, blocks = ?9, entities = ?10, metadata = ?11, fingerprint = ?12,
                         state = 'open', snoozed_until = NULL, score = NULL, base_score = NULL,
                         ai_adjustment = NULL, urgency = NULL, reason = NULL, ranked_by = NULL, updated_at = ?13
                     WHERE id = ?1",
                    params![
                        id, e.kind, e.title, e.body, e.url, e.actor, e.context, ts(e.occurred_at),
                        blocks, entities, metadata, fp, ts(now)
                    ],
                )?;
                (id, false)
            }
            None => {
                let id = uuid::Uuid::new_v4().to_string();
                tx.execute(
                    "INSERT INTO items (id, source, external_id, kind, title, body, url, actor, context,
                         occurred_at, blocks, entities, metadata, fingerprint, state, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, 'open', ?15, ?15)",
                    params![
                        id, e.source, e.external_id, e.kind, e.title, e.body, e.url, e.actor, e.context,
                        ts(e.occurred_at), blocks, entities, metadata, fp, ts(now)
                    ],
                )?;
                (id, true)
            }
        };
        super::graph::link_item(&tx, &id, &e.entities, now)?;
        let item = get_item(&tx, &id)?.expect("row just written");
        Transaction::commit(tx)?;
        Ok(if inserted {
            Upsert::Inserted(item)
        } else {
            Upsert::Updated(item)
        })
    }

    pub fn get(&self, id: &str) -> anyhow::Result<Option<QueueItem>> {
        get_item(&self.conn(), id)
    }

    /// The newest item for a web address. `url` should be canonical (no
    /// query or fragment); an item whose address continues past it with
    /// `/`, `#` or `?` (a PR's `/files` page, a comment anchor) matches too.
    pub fn item_by_url(&self, url: &str) -> anyhow::Result<Option<QueueItem>> {
        item_by_url(&self.conn(), url)
    }

    /// Every source that has items stored.
    pub fn sources(&self) -> anyhow::Result<Vec<String>> {
        let conn = self.conn();
        let rows = conn
            .prepare("SELECT DISTINCT source FROM items")?
            .query_map([], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// Deletes every item a source brought in (its context, previews and links
    /// go with it), then any graph entities nothing points at any more.
    /// Returns how many items were removed.
    pub fn remove_source(&self, source: &str) -> anyhow::Result<usize> {
        let conn = self.conn();
        let n = conn.execute("DELETE FROM items WHERE source = ?1", [source])?;
        if n > 0 {
            conn.execute(
                "DELETE FROM entities WHERE id NOT IN (SELECT entity_id FROM item_entities)",
                [],
            )?;
        }
        Ok(n)
    }

    pub fn set_ranking(&self, id: &str, r: &Ranking) -> anyhow::Result<Option<QueueItem>> {
        let conn = self.conn();
        conn.execute(
            "UPDATE items SET score = ?2, base_score = ?3, ai_adjustment = ?4, urgency = ?5, reason = ?6,
                 ranked_by = ?7
             WHERE id = ?1",
            params![id, r.score, r.base_score, r.ai_adjustment, r.urgency.as_str(), r.reason, r.ranked_by],
        )?;
        get_item(&conn, id)
    }

    /// Drops every ranking so the engine recomputes them (after rules or instructions change).
    pub fn clear_rankings(&self) -> anyhow::Result<usize> {
        Ok(self.conn().execute(
            "UPDATE items SET score = NULL, base_score = NULL, ai_adjustment = NULL, urgency = NULL,
                 reason = NULL, ranked_by = NULL
             WHERE state != 'done'",
            [],
        )?)
    }

    /// The open or snoozed item for a source's external id, if any.
    pub fn find_active(
        &self,
        source: &str,
        external_id: &str,
    ) -> anyhow::Result<Option<QueueItem>> {
        let conn = self.conn();
        let id: Option<String> = conn
            .query_row(
                "SELECT id FROM items WHERE source = ?1 AND external_id = ?2 AND state IN ('open', 'snoozed')",
                params![source, external_id],
                |r| r.get(0),
            )
            .optional()?;
        match id {
            Some(id) => get_item(&conn, &id),
            None => Ok(None),
        }
    }

    pub fn set_state(
        &self,
        id: &str,
        state: ItemState,
        snoozed_until: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
    ) -> anyhow::Result<Option<QueueItem>> {
        let conn = self.conn();
        conn.execute(
            "UPDATE items SET state = ?2, snoozed_until = ?3, updated_at = ?4 WHERE id = ?1",
            params![id, state.as_str(), snoozed_until.map(ts), ts(now)],
        )?;
        get_item(&conn, id)
    }

    /// Reopens snoozed items whose time has come and returns them.
    pub fn wake_due(&self, now: DateTime<Utc>) -> anyhow::Result<Vec<QueueItem>> {
        let conn = self.conn();
        let ids: Vec<String> = conn
            .prepare("SELECT id FROM items WHERE state = 'snoozed' AND snoozed_until <= ?1")?
            .query_map([ts(now)], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            conn.execute(
                "UPDATE items SET state = 'open', snoozed_until = NULL, updated_at = ?2 WHERE id = ?1",
                params![id, ts(now)],
            )?;
            out.extend(get_item(&conn, &id)?);
        }
        Ok(out)
    }

    /// Items that still need ranking (new, changed, or the ranker was down).
    pub fn unranked(&self) -> anyhow::Result<Vec<QueueItem>> {
        let conn = self.conn();
        let items = conn
            .prepare(&format!(
                "SELECT {COLUMNS} FROM items WHERE score IS NULL AND state != 'done' {ORDER}"
            ))?
            .query_map([], row_to_item)?
            .collect::<Result<_, _>>()?;
        Ok(items)
    }

    /// The queue in display order.
    pub fn list(&self, states: &[ItemState]) -> anyhow::Result<Vec<QueueItem>> {
        let conn = self.conn();
        let sql = format!(
            "SELECT {COLUMNS} FROM items WHERE state IN ({}) {ORDER} LIMIT 1000",
            state_list(states)
        );
        let items = conn
            .prepare(&sql)?
            .query_map([], row_to_item)?
            .collect::<Result<_, _>>()?;
        Ok(items)
    }

    /// Substring search over title, body, actor, context and kind, best matches first.
    pub fn search(
        &self,
        query: &str,
        states: &[ItemState],
        limit: usize,
    ) -> anyhow::Result<Vec<QueueItem>> {
        self.search_in(query, states, &Scope::default(), limit)
    }

    /// Keyword search limited to a time window and/or a project.
    pub fn search_in(
        &self,
        query: &str,
        states: &[ItemState],
        scope: &Scope,
        limit: usize,
    ) -> anyhow::Result<Vec<QueueItem>> {
        let conn = self.conn();
        let states = state_list(states);
        // Unset bounds match everything; `occurred_at` is indexed.
        let window =
            "AND (?3 IS NULL OR items.occurred_at >= ?3) AND (?4 IS NULL OR items.occurred_at < ?4)
                      AND (?5 IS NULL OR items.project_id = ?5)";
        let (since, until) = (scope.since.map(ts), scope.until.map(ts));
        let items = match fts_phrase(query) {
            Some(phrase) => conn
                .prepare(&format!(
                    "SELECT {COLUMNS} FROM items_fts JOIN items ON items.rowid = items_fts.rowid
                     WHERE items_fts MATCH ?1 AND items.state IN ({states}) {window}
                     ORDER BY bm25(items_fts, 10.0, 2.0, 4.0, 4.0, 1.0), items.id LIMIT ?2"
                ))?
                .query_map(params![phrase, limit, since, until, scope.project_id], row_to_item)?
                .collect::<Result<_, _>>()?,
            None => conn
                .prepare(&format!(
                    "SELECT {COLUMNS} FROM items WHERE title LIKE ?1 AND state IN ({states}) {window} {ORDER} LIMIT ?2"
                ))?
                .query_map(
                    params![format!("%{}%", query.trim()), limit, since, until, scope.project_id],
                    row_to_item,
                )?
                .collect::<Result<_, _>>()?,
        };
        Ok(items)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::EntityRef;
    use chrono::Duration;

    pub(crate) fn ev(id: &str, title: &str) -> IncomingEvent {
        IncomingEvent {
            source: "test".into(),
            external_id: id.into(),
            kind: "test.thing".into(),
            title: title.into(),
            occurred_at: DateTime::parse_from_rfc3339("2026-11-04T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            metadata: serde_json::json!({"n": 1}),
            entities: vec![EntityRef::new("person", "dana", "Dana", "from")],
            ..Default::default()
        }
    }

    #[test]
    fn item_by_url_matches_subpages_only() {
        let s = Store::in_memory().unwrap();
        let now = Utc::now();
        let mut e = ev("1", "PR");
        e.url = Some("https://github.com/o/r/pull/12".into());
        s.upsert(&e, now).unwrap();
        let mut e2 = ev("2", "other PR");
        e2.url = Some("https://github.com/o/r/pull/123".into());
        s.upsert(&e2, now).unwrap();
        let hit = |u: &str| s.item_by_url(u).unwrap().map(|i| i.event.title);
        assert_eq!(hit("https://github.com/o/r/pull/12").as_deref(), Some("PR"));
        assert_eq!(
            hit("https://github.com/o/r/pull/12/").as_deref(),
            Some("PR")
        );
        assert_eq!(
            hit("https://github.com/o/r/pull/123").as_deref(),
            Some("other PR")
        );
        assert_eq!(hit("https://github.com/o/r/pull/1"), None);
        assert_eq!(hit(""), None);
    }

    fn rank(score: u8) -> Ranking {
        Ranking {
            score,
            base_score: score,
            ai_adjustment: None,
            urgency: Urgency::from_score(score),
            reason: "r".into(),
            ranked_by: "rules".into(),
        }
    }

    fn insert(s: &Store, id: &str, title: &str) -> String {
        match s.upsert(&ev(id, title), Utc::now()).unwrap() {
            Upsert::Inserted(i) => i.id,
            other => panic!("expected insert, got {other:?}"),
        }
    }

    #[test]
    fn upsert_dedupes_and_reopens_on_change() {
        let s = Store::in_memory().unwrap();
        let now = Utc::now();
        let id = insert(&s, "1", "a");
        assert_eq!(s.upsert(&ev("1", "a"), now).unwrap(), Upsert::Unchanged);

        s.set_ranking(&id, &rank(70)).unwrap();
        s.set_state(&id, ItemState::Done, None, now).unwrap();
        let Upsert::Updated(again) = s.upsert(&ev("1", "a, with a new comment"), now).unwrap()
        else {
            panic!()
        };
        assert_eq!(again.id, id);
        assert_eq!(again.state, ItemState::Open);
        assert!(again.ranking.is_none(), "changed items need re-ranking");
        assert_eq!(again.event.metadata["n"], 1);
        assert_eq!(again.event.entities[0].key, "dana");
    }

    #[test]
    fn list_order_is_deterministic_and_snooze_wakes() {
        let s = Store::in_memory().unwrap();
        let now = Utc::now();
        let low = insert(&s, "low", "low");
        let high = insert(&s, "high", "high");
        let mid = insert(&s, "mid", "mid");
        s.set_ranking(&low, &rank(10)).unwrap();
        s.set_ranking(&high, &rank(90)).unwrap();
        s.set_ranking(&mid, &rank(60)).unwrap();
        let titles: Vec<_> = s
            .list(&[ItemState::Open])
            .unwrap()
            .into_iter()
            .map(|i| i.event.title)
            .collect();
        assert_eq!(titles, ["high", "mid", "low"]);

        s.set_state(
            &high,
            ItemState::Snoozed,
            Some(now + Duration::minutes(5)),
            now,
        )
        .unwrap();
        assert_eq!(s.list(&[ItemState::Open]).unwrap().len(), 2);
        assert!(s.wake_due(now).unwrap().is_empty());
        assert_eq!(s.wake_due(now + Duration::minutes(6)).unwrap().len(), 1);
        assert_eq!(s.list(&[ItemState::Open]).unwrap()[0].event.title, "high");
    }

    #[test]
    fn trigram_search_finds_substrings_and_follows_updates() {
        let s = Store::in_memory().unwrap();
        insert(&s, "1", "Latency spike in #incident-checkout");
        insert(&s, "2", "Review: idempotency keys for webhooks");
        let hits = |q: &str| -> Vec<String> {
            s.search(q, &[ItemState::Open], 10)
                .unwrap()
                .into_iter()
                .map(|i| i.event.external_id)
                .collect()
        };
        assert_eq!(hits("checko"), ["1"]);
        assert_eq!(hits("IDEMPOTENCY"), ["2"]);
        assert_eq!(
            hits("we"),
            ["2"],
            "short queries fall back to LIKE on title"
        );
        assert!(hits("nothing like this").is_empty());

        s.upsert(&ev("2", "Review: retry budget for webhooks"), Utc::now())
            .unwrap();
        assert!(hits("idempotency").is_empty(), "FTS index follows updates");
        assert_eq!(hits("retry budget"), ["2"]);
    }
}
