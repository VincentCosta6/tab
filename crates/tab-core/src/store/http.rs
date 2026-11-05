//! Storage for the gateway: HTTP response cache, AI result cache, audit log.

use chrono::{DateTime, Duration, Utc};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use super::{Store, parse_ts, ts};

#[derive(Debug, Clone, PartialEq)]
pub struct CachedResponse {
    pub status: u16,
    pub body: String,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub fetched_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AuditEntry {
    #[serde(default)]
    pub id: i64,
    pub ts: DateTime<Utc>,
    pub provider: String,
    pub connector: String,
    pub purpose: String,
    pub method: String,
    pub url: String,
    /// `network`, `cache_hit`, `revalidated`, `stale`, `rate_limited` or `error`.
    pub outcome: String,
    pub status: Option<u16>,
    pub duration_ms: u64,
    pub rl_resource: Option<String>,
    pub rl_limit: Option<u64>,
    pub rl_remaining: Option<u64>,
    pub rl_reset: Option<DateTime<Utc>>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuditQuery {
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub connector: Option<String>,
    #[serde(default)]
    pub outcome: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

/// Calls grouped by who made them and why, for the usage table.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UsageRow {
    pub provider: String,
    pub connector: String,
    pub purpose: String,
    pub total: u64,
    /// Calls that actually hit the provider's API and could count against a limit.
    pub network: u64,
    pub cache_hits: u64,
    pub revalidated: u64,
    pub stale: u64,
    pub rate_limited: u64,
    pub errors: u64,
    pub avg_ms: f64,
}

impl Store {
    pub fn cache_get(&self, key: &str) -> anyhow::Result<Option<CachedResponse>> {
        let conn = self.conn();
        Ok(conn
            .query_row(
                "SELECT status, body, etag, last_modified, fetched_at, expires_at FROM http_cache WHERE key = ?1",
                [key],
                |r| {
                    Ok(CachedResponse {
                        status: r.get(0)?,
                        body: r.get(1)?,
                        etag: r.get(2)?,
                        last_modified: r.get(3)?,
                        fetched_at: parse_ts(&r.get::<_, String>(4)?)?,
                        expires_at: parse_ts(&r.get::<_, String>(5)?)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn cache_put(
        &self,
        key: &str,
        provider: &str,
        url: &str,
        c: &CachedResponse,
    ) -> anyhow::Result<()> {
        self.conn().execute(
            "INSERT INTO http_cache (key, provider, url, status, body, etag, last_modified, fetched_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT (key) DO UPDATE SET status = excluded.status, body = excluded.body, etag = excluded.etag,
                 last_modified = excluded.last_modified, fetched_at = excluded.fetched_at,
                 expires_at = excluded.expires_at",
            params![key, provider, url, c.status, c.body, c.etag, c.last_modified, ts(c.fetched_at), ts(c.expires_at)],
        )?;
        Ok(())
    }

    /// Extends a cached entry after a `304 Not Modified`.
    pub fn cache_touch(
        &self,
        key: &str,
        fetched_at: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    ) -> anyhow::Result<()> {
        self.conn().execute(
            "UPDATE http_cache SET fetched_at = ?2, expires_at = ?3 WHERE key = ?1",
            params![key, ts(fetched_at), ts(expires_at)],
        )?;
        Ok(())
    }

    pub fn ai_cache_get(&self, key: &str) -> anyhow::Result<Option<String>> {
        Ok(self
            .conn()
            .query_row("SELECT value FROM ai_cache WHERE key = ?1", [key], |r| {
                r.get(0)
            })
            .optional()?)
    }

    pub fn ai_cache_put(&self, key: &str, model: &str, value: &str) -> anyhow::Result<()> {
        self.conn().execute(
            "INSERT OR REPLACE INTO ai_cache (key, model, value, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![key, model, value, ts(Utc::now())],
        )?;
        Ok(())
    }

    pub fn audit_insert(&self, a: &AuditEntry) -> anyhow::Result<()> {
        self.conn().execute(
            "INSERT INTO api_audit (ts, provider, connector, purpose, method, url, outcome, status, duration_ms,
                 rl_resource, rl_limit, rl_remaining, rl_reset, error)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                ts(a.ts), a.provider, a.connector, a.purpose, a.method, a.url, a.outcome, a.status,
                a.duration_ms as i64, a.rl_resource, a.rl_limit.map(|v| v as i64),
                a.rl_remaining.map(|v| v as i64), a.rl_reset.map(ts), a.error
            ],
        )?;
        Ok(())
    }

    pub fn audit_list(&self, q: &AuditQuery) -> anyhow::Result<Vec<AuditEntry>> {
        let conn = self.conn();
        let rows = conn
            .prepare(
                "SELECT id, ts, provider, connector, purpose, method, url, outcome, status, duration_ms,
                     rl_resource, rl_limit, rl_remaining, rl_reset, error
                 FROM api_audit
                 WHERE (?1 IS NULL OR provider = ?1) AND (?2 IS NULL OR connector = ?2) AND (?3 IS NULL OR outcome = ?3)
                 ORDER BY ts DESC, id DESC LIMIT ?4",
            )?
            .query_map(
                params![q.provider, q.connector, q.outcome, q.limit.unwrap_or(200).min(5000)],
                |r| {
                    Ok(AuditEntry {
                        id: r.get(0)?,
                        ts: parse_ts(&r.get::<_, String>(1)?)?,
                        provider: r.get(2)?,
                        connector: r.get(3)?,
                        purpose: r.get(4)?,
                        method: r.get(5)?,
                        url: r.get(6)?,
                        outcome: r.get(7)?,
                        status: r.get(8)?,
                        duration_ms: r.get::<_, i64>(9)? as u64,
                        rl_resource: r.get(10)?,
                        rl_limit: r.get::<_, Option<i64>>(11)?.map(|v| v as u64),
                        rl_remaining: r.get::<_, Option<i64>>(12)?.map(|v| v as u64),
                        rl_reset: r.get::<_, Option<String>>(13)?.as_deref().map(parse_ts).transpose()?,
                        error: r.get(14)?,
                    })
                },
            )?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    pub fn usage_since(&self, since: DateTime<Utc>) -> anyhow::Result<Vec<UsageRow>> {
        let conn = self.conn();
        let rows = conn
            .prepare(
                "SELECT provider, connector, purpose, COUNT(*),
                     SUM(outcome = 'network'), SUM(outcome = 'cache_hit'), SUM(outcome = 'revalidated'),
                     SUM(outcome = 'stale'), SUM(outcome = 'rate_limited'), SUM(outcome = 'error'),
                     AVG(CASE WHEN outcome IN ('network', 'revalidated', 'error') THEN duration_ms END)
                 FROM api_audit WHERE ts >= ?1
                 GROUP BY provider, connector, purpose
                 ORDER BY SUM(outcome = 'network') DESC, COUNT(*) DESC, provider, connector, purpose",
            )?
            .query_map([ts(since)], |r| {
                Ok(UsageRow {
                    provider: r.get(0)?,
                    connector: r.get(1)?,
                    purpose: r.get(2)?,
                    total: r.get::<_, i64>(3)? as u64,
                    network: r.get::<_, i64>(4)? as u64,
                    cache_hits: r.get::<_, i64>(5)? as u64,
                    revalidated: r.get::<_, i64>(6)? as u64,
                    stale: r.get::<_, i64>(7)? as u64,
                    rate_limited: r.get::<_, i64>(8)? as u64,
                    errors: r.get::<_, i64>(9)? as u64,
                    avg_ms: r.get::<_, Option<f64>>(10)?.unwrap_or(0.0),
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// Housekeeping: drop audit rows older than `keep` and expired cache entries older than a week.
    pub fn prune(&self, now: DateTime<Utc>, keep: Duration) -> anyhow::Result<()> {
        let conn = self.conn();
        conn.execute("DELETE FROM api_audit WHERE ts < ?1", [ts(now - keep)])?;
        conn.execute(
            "DELETE FROM http_cache WHERE expires_at < ?1",
            [ts(now - Duration::days(7))],
        )?;
        Ok(())
    }
}
