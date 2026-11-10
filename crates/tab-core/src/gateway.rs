//! Every outbound API call goes through the [`Gateway`].
//!
//! It gives all connectors (and the AI ranker) the same behavior for free:
//!
//! * **Cache.** GET responses can be cached with a TTL. Rich previews read
//!   from here, so opening an item rarely costs an API call. Stale entries
//!   are revalidated with `If-None-Match` / `If-Modified-Since`; GitHub does
//!   not count a `304` against the rate limit.
//! * **Rate limits.** Limit headers (`x-ratelimit-*`, `anthropic-ratelimit-*`,
//!   `retry-after`) are tracked per provider and resource. Once a limit is hit
//!   the gateway refuses further calls locally until the reset time, serving
//!   stale cache when it has some.
//! * **Audit.** One row per call attempt, including cache hits and local
//!   refusals: who called (connector), why (purpose), what, the outcome, and
//!   the limit state the provider reported.
//! * **Offline.** Network errors fall back to stale cache too.
//! * **Budgets.** Connectors declare their limits; the [`Limiter`] tracks
//!   usage and bursts so background work backs off before the provider
//!   has to tell us to (see [`crate::ratelimit`]).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, TimeZone, Utc};
use reqwest::header::{
    ETAG, HeaderMap, HeaderName, HeaderValue, IF_MODIFIED_SINCE, IF_NONE_MATCH, LAST_MODIFIED,
    RETRY_AFTER,
};
use reqwest::{Method, StatusCode};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;

use crate::ratelimit::{Limiter, Pressure, Priority, RateLimitSpec, Refusal, current_priority};
use crate::store::{AuditEntry, CachedResponse, Store};

const MAX_CONCURRENT_PER_PROVIDER: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Went to the provider and got fresh data.
    Network,
    /// Served from a fresh cache entry; no request made.
    CacheHit,
    /// Provider said `304 Not Modified`; served the cached body.
    Revalidated,
    /// Served an expired cache entry because of a rate limit or network error.
    Stale,
    /// Refused locally or by the provider because of a rate limit.
    RateLimited,
    /// Background call held back to keep headroom; nothing was sent.
    Deferred,
    Error,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Network => "network",
            Outcome::CacheHit => "cache_hit",
            Outcome::Revalidated => "revalidated",
            Outcome::Stale => "stale",
            Outcome::RateLimited => "rate_limited",
            Outcome::Deferred => "deferred",
            Outcome::Error => "error",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum GatewayError {
    #[error("{provider} rate limit ({resource}) exhausted until {until}")]
    RateLimited {
        provider: String,
        resource: String,
        until: DateTime<Utc>,
    },
    #[error("{provider} background call deferred ({reason}) until {until}")]
    Deferred {
        provider: String,
        resource: String,
        reason: String,
        until: DateTime<Utc>,
    },
    #[error("{provider} returned {status}: {body}")]
    Http {
        provider: String,
        status: u16,
        body: String,
    },
    #[error("request to {provider} failed: {message}")]
    Network { provider: String, message: String },
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// One API call, described by who is making it and why.
#[derive(Debug, Clone)]
pub struct Call {
    pub provider: String,
    pub connector: String,
    pub purpose: String,
    pub method: Method,
    pub url: String,
    pub query: Vec<(String, String)>,
    pub headers: Vec<(HeaderName, HeaderValue)>,
    pub json: Option<serde_json::Value>,
    /// Cache GET responses for this long. `None` means never cache.
    pub cache_ttl: Option<Duration>,
}

impl Call {
    pub fn new(
        method: Method,
        provider: &str,
        connector: &str,
        purpose: &str,
        url: impl Into<String>,
    ) -> Self {
        Self {
            provider: provider.into(),
            connector: connector.into(),
            purpose: purpose.into(),
            method,
            url: url.into(),
            query: Vec::new(),
            headers: Vec::new(),
            json: None,
            cache_ttl: None,
        }
    }
    pub fn get(provider: &str, connector: &str, purpose: &str, url: impl Into<String>) -> Self {
        Self::new(Method::GET, provider, connector, purpose, url)
    }
    pub fn post(provider: &str, connector: &str, purpose: &str, url: impl Into<String>) -> Self {
        Self::new(Method::POST, provider, connector, purpose, url)
    }
    pub fn query(mut self, k: &str, v: impl Into<String>) -> Self {
        self.query.push((k.into(), v.into()));
        self
    }
    pub fn header(mut self, k: HeaderName, v: HeaderValue) -> Self {
        self.headers.push((k, v));
        self
    }
    pub fn json(mut self, body: serde_json::Value) -> Self {
        self.json = Some(body);
        self
    }
    pub fn cache(mut self, ttl: Duration) -> Self {
        self.cache_ttl = Some(ttl);
        self
    }
}

#[derive(Debug, Clone)]
pub struct Response {
    pub status: u16,
    pub body: String,
    pub outcome: Outcome,
    /// Headers from the network response; empty when served from cache.
    pub headers: HeaderMap,
}

impl Response {
    pub fn json<T: serde::de::DeserializeOwned>(&self) -> anyhow::Result<T> {
        Ok(serde_json::from_str(&self.body)?)
    }
    /// True when the data is the same as what we already had.
    pub fn unchanged(&self) -> bool {
        matches!(
            self.outcome,
            Outcome::CacheHit | Outcome::Revalidated | Outcome::Stale
        )
    }
}

/// What the provider last told us about one rate-limit bucket.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RateLimitState {
    pub provider: String,
    pub resource: String,
    pub limit: Option<u64>,
    pub remaining: Option<u64>,
    pub reset_at: Option<DateTime<Utc>>,
    /// Set after a 429 / secondary limit; no calls go out until then.
    pub blocked_until: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}

impl RateLimitState {
    fn blocked(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        if let Some(until) = self.blocked_until.filter(|u| *u > now) {
            return Some(until);
        }
        match (self.remaining, self.reset_at) {
            (Some(0), Some(reset)) if reset > now => Some(reset),
            _ => None,
        }
    }
}

/// Which limit bucket a URL draws from. GitHub meters search and GraphQL separately.
fn resource_for(provider: &str, url: &str) -> &'static str {
    match provider {
        "github" if url.contains("/search/") => "search",
        "github" if url.ends_with("/graphql") => "graphql",
        "github" => "core",
        "anthropic" => "requests",
        _ => "default",
    }
}

/// Reads any of the common rate-limit header families.
fn parse_limits(
    h: &HeaderMap,
    now: DateTime<Utc>,
) -> (
    Option<u64>,
    Option<u64>,
    Option<DateTime<Utc>>,
    Option<String>,
) {
    let num = |k: &str| h.get(k)?.to_str().ok()?.trim().parse::<u64>().ok();
    let text = |k: &str| h.get(k)?.to_str().ok().map(str::to_string);
    if let Some(remaining) = num("x-ratelimit-remaining") {
        // GitHub and many others: reset is a unix epoch.
        let reset = num("x-ratelimit-reset").and_then(|s| Utc.timestamp_opt(s as i64, 0).single());
        return (
            num("x-ratelimit-limit"),
            Some(remaining),
            reset,
            text("x-ratelimit-resource"),
        );
    }
    if let Some(remaining) = num("anthropic-ratelimit-requests-remaining") {
        let reset = text("anthropic-ratelimit-requests-reset")
            .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
            .map(|t| t.with_timezone(&Utc));
        return (
            num("anthropic-ratelimit-requests-limit"),
            Some(remaining),
            reset,
            Some("requests".into()),
        );
    }
    let _ = now;
    (None, None, None, None)
}

fn retry_after(h: &HeaderMap, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let secs: i64 = h.get(RETRY_AFTER)?.to_str().ok()?.trim().parse().ok()?;
    Some(now + chrono::Duration::seconds(secs.max(1)))
}

/// URL for the audit log with secret-looking query values masked.
fn redact(url: &reqwest::Url) -> String {
    let mut u = url.clone();
    let pairs: Vec<(String, String)> = url
        .query_pairs()
        .map(|(k, v)| {
            let lk = k.to_lowercase();
            let secret = ["token", "key", "secret", "sig", "password", "auth"]
                .iter()
                .any(|s| lk.contains(s));
            (
                k.into_owned(),
                if secret { "***".into() } else { v.into_owned() },
            )
        })
        .collect();
    if pairs.is_empty() {
        u.set_query(None);
    } else {
        u.query_pairs_mut().clear().extend_pairs(pairs);
    }
    u.to_string()
}

fn cache_key(provider: &str, method: &Method, url: &reqwest::Url) -> String {
    let digest = Sha256::digest(format!("{provider}\n{method}\n{url}"));
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

pub struct Gateway {
    store: Arc<Store>,
    client: reqwest::Client,
    /// Default headers per provider (auth, user agent), set by connectors.
    provider_headers: Mutex<HashMap<String, HeaderMap>>,
    limits: Mutex<HashMap<(String, String), RateLimitState>>,
    slots: Mutex<HashMap<String, Arc<Semaphore>>>,
    limiter: Limiter,
}

impl Gateway {
    pub fn new(store: Arc<Store>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .user_agent("tab-desktop/0.1")
            .build()
            .expect("reqwest client");
        Self {
            store,
            client,
            provider_headers: Mutex::new(HashMap::new()),
            limits: Mutex::new(HashMap::new()),
            slots: Mutex::new(HashMap::new()),
            limiter: Limiter::default(),
        }
    }

    /// Registers headers sent with every call to a provider (e.g. auth).
    pub fn set_provider_headers(&self, provider: &str, headers: HeaderMap) {
        self.provider_headers
            .lock()
            .unwrap()
            .insert(provider.into(), headers);
    }

    /// Declares limits a connector knows about. Called by the engine for
    /// every registered connector, so each one gets budgets for free.
    pub fn declare_limits(&self, specs: &[RateLimitSpec]) {
        self.limiter.declare(specs);
    }

    fn remote(&self, provider: &str, resource: &str) -> Option<(u64, Option<u64>)> {
        let all = self.limits.lock().unwrap();
        let st = all.get(&(provider.to_string(), resource.to_string()))?;
        Some((st.remaining?, st.limit))
    }

    /// Usage, headroom and burst state per bucket.
    pub fn pressure(&self) -> Vec<Pressure> {
        self.limiter.pressure(|p, r| self.remote(p, r))
    }

    /// Whether a background call to this bucket would go out right now.
    pub fn has_headroom(&self, provider: &str, resource: &str) -> bool {
        // Admission with no record: a dry run.
        self.limiter
            .admit(
                provider,
                resource,
                Priority::Background,
                self.remote(provider, resource),
            )
            .is_ok()
    }

    pub fn rate_limits(&self) -> Vec<RateLimitState> {
        let mut v: Vec<_> = self.limits.lock().unwrap().values().cloned().collect();
        v.sort_by(|a, b| (&a.provider, &a.resource).cmp(&(&b.provider, &b.resource)));
        v
    }

    fn slot(&self, provider: &str) -> Arc<Semaphore> {
        self.slots
            .lock()
            .unwrap()
            .entry(provider.into())
            .or_insert_with(|| Arc::new(Semaphore::new(MAX_CONCURRENT_PER_PROVIDER)))
            .clone()
    }

    fn audit(
        &self,
        call: &Call,
        url: &str,
        outcome: Outcome,
        status: Option<u16>,
        started: Instant,
        error: Option<String>,
    ) {
        let limit = self
            .limits
            .lock()
            .unwrap()
            .get(&(
                call.provider.clone(),
                resource_for(&call.provider, &call.url).to_string(),
            ))
            .cloned();
        let entry = AuditEntry {
            id: 0,
            ts: Utc::now(),
            provider: call.provider.clone(),
            connector: call.connector.clone(),
            purpose: call.purpose.clone(),
            method: call.method.to_string(),
            url: url.to_string(),
            outcome: outcome.as_str().into(),
            status,
            duration_ms: started.elapsed().as_millis() as u64,
            rl_resource: limit.as_ref().map(|l| l.resource.clone()),
            rl_limit: limit.as_ref().and_then(|l| l.limit),
            rl_remaining: limit.as_ref().and_then(|l| l.remaining),
            rl_reset: limit.as_ref().and_then(|l| l.blocked_until.or(l.reset_at)),
            error,
        };
        if let Err(e) = self.store.audit_insert(&entry) {
            tracing::error!(error = %e, "writing audit log failed");
        }
        tracing::debug!(provider = %call.provider, connector = %call.connector, purpose = %call.purpose, outcome = outcome.as_str(), "api call");
    }

    fn record_limits(
        &self,
        provider: &str,
        resource: &str,
        h: &HeaderMap,
        now: DateTime<Utc>,
        blocked_until: Option<DateTime<Utc>>,
    ) {
        let (limit, remaining, reset_at, reported) = parse_limits(h, now);
        if remaining.is_none() && blocked_until.is_none() {
            return;
        }
        let resource = reported.unwrap_or_else(|| resource.to_string());
        let mut all = self.limits.lock().unwrap();
        let st = all
            .entry((provider.to_string(), resource.clone()))
            .or_insert(RateLimitState {
                provider: provider.into(),
                resource,
                limit: None,
                remaining: None,
                reset_at: None,
                blocked_until: None,
                updated_at: now,
            });
        if remaining.is_some() {
            st.limit = limit.or(st.limit);
            st.remaining = remaining;
            st.reset_at = reset_at.or(st.reset_at);
        }
        if blocked_until.is_some() {
            st.blocked_until = blocked_until;
        }
        st.updated_at = now;
    }

    fn stale_or(
        &self,
        call: &Call,
        url: &str,
        cached: Option<CachedResponse>,
        started: Instant,
        err: GatewayError,
    ) -> Result<Response, GatewayError> {
        match cached {
            Some(c) => {
                self.audit(
                    call,
                    url,
                    Outcome::Stale,
                    Some(c.status),
                    started,
                    Some(err.to_string()),
                );
                Ok(Response {
                    status: c.status,
                    body: c.body,
                    outcome: Outcome::Stale,
                    headers: HeaderMap::new(),
                })
            }
            None => {
                let outcome = match err {
                    GatewayError::RateLimited { .. } => Outcome::RateLimited,
                    GatewayError::Deferred { .. } => Outcome::Deferred,
                    _ => Outcome::Error,
                };
                let status = match &err {
                    GatewayError::Http { status, .. } => Some(*status),
                    _ => None,
                };
                self.audit(call, url, outcome, status, started, Some(err.to_string()));
                Err(err)
            }
        }
    }

    /// Runs a call that doesn't go over plain HTTP from here (a tool call
    /// to an MCP server, which [`crate::remote_mcp`] signs in and sends)
    /// under the same budgets as [`Self::send`], and logs it. `call` says who
    /// and why; its `url` is what to log. Nothing is cached.
    pub async fn run<T>(
        &self,
        call: Call,
        f: impl std::future::Future<Output = anyhow::Result<T>>,
    ) -> Result<T, GatewayError> {
        let started = Instant::now();
        let resource = resource_for(&call.provider, &call.url);
        let refused = match self.limiter.admit(
            &call.provider,
            resource,
            current_priority(),
            self.remote(&call.provider, resource),
        ) {
            Ok(()) => None,
            Err(Refusal::Exhausted { until }) => Some(GatewayError::RateLimited {
                provider: call.provider.clone(),
                resource: resource.into(),
                until,
            }),
            Err(Refusal::Deferred { reason, until }) => Some(GatewayError::Deferred {
                provider: call.provider.clone(),
                resource: resource.into(),
                reason,
                until,
            }),
        };
        if let Some(err) = refused {
            let outcome = match err {
                GatewayError::RateLimited { .. } => Outcome::RateLimited,
                _ => Outcome::Deferred,
            };
            self.audit(
                &call,
                &call.url,
                outcome,
                None,
                started,
                Some(err.to_string()),
            );
            return Err(err);
        }
        let slot = self.slot(&call.provider);
        let _permit = slot
            .acquire()
            .await
            .map_err(|e| GatewayError::Other(e.into()))?;
        self.limiter.record(&call.provider, resource);
        match f.await {
            Ok(v) => {
                self.audit(&call, &call.url, Outcome::Network, None, started, None);
                Ok(v)
            }
            Err(e) => {
                self.audit(
                    &call,
                    &call.url,
                    Outcome::Error,
                    None,
                    started,
                    Some(e.to_string()),
                );
                Err(GatewayError::Other(e))
            }
        }
    }

    pub async fn send(&self, call: Call) -> Result<Response, GatewayError> {
        let started = Instant::now();
        let now = Utc::now();
        let resource = resource_for(&call.provider, &call.url);

        let mut req = self
            .client
            .request(call.method.clone(), &call.url)
            .query(&call.query);
        if let Some(h) = self.provider_headers.lock().unwrap().get(&call.provider) {
            req = req.headers(h.clone());
        }
        for (k, v) in &call.headers {
            req = req.header(k, v);
        }
        if let Some(body) = &call.json {
            req = req.json(body);
        }
        let mut req = req.build().map_err(|e| GatewayError::Other(e.into()))?;
        let audit_url = redact(req.url());

        // 1. Fresh cache answers without touching the network.
        let cacheable = call.method == Method::GET && call.cache_ttl.is_some();
        let key = cacheable.then(|| cache_key(&call.provider, &call.method, req.url()));
        let cached = match &key {
            Some(k) => self.store.cache_get(k)?,
            None => None,
        };
        // Freshness is decided by the caller's TTL, so a preview can accept
        // five-minute-old data while a poll always revalidates the same URL.
        let max_age = call
            .cache_ttl
            .map(|d| chrono::Duration::from_std(d).unwrap_or_default())
            .unwrap_or_default();
        if let Some(c) = cached.as_ref().filter(|c| now < c.fetched_at + max_age) {
            self.audit(
                &call,
                &audit_url,
                Outcome::CacheHit,
                Some(c.status),
                started,
                None,
            );
            return Ok(Response {
                status: c.status,
                body: c.body.clone(),
                outcome: Outcome::CacheHit,
                headers: HeaderMap::new(),
            });
        }

        // 2. Known-exhausted limits are refused locally.
        let blocked = self
            .limits
            .lock()
            .unwrap()
            .get(&(call.provider.clone(), resource.to_string()))
            .and_then(|l| l.blocked(now));
        if let Some(until) = blocked {
            let err = GatewayError::RateLimited {
                provider: call.provider.clone(),
                resource: resource.into(),
                until,
            };
            return self.stale_or(&call, &audit_url, cached, started, err);
        }

        // 3. Declared budgets, usage and bursts.
        let remote = self.remote(&call.provider, resource);
        match self
            .limiter
            .admit(&call.provider, resource, current_priority(), remote)
        {
            Ok(()) => {}
            Err(Refusal::Exhausted { until }) => {
                let err = GatewayError::RateLimited {
                    provider: call.provider.clone(),
                    resource: resource.into(),
                    until,
                };
                return self.stale_or(&call, &audit_url, cached, started, err);
            }
            Err(Refusal::Deferred { reason, until }) => {
                let err = GatewayError::Deferred {
                    provider: call.provider.clone(),
                    resource: resource.into(),
                    reason,
                    until,
                };
                return self.stale_or(&call, &audit_url, cached, started, err);
            }
        }

        // 4. Conditional request when we have something to revalidate.
        if let Some(c) = &cached {
            if let Some(v) = c
                .etag
                .as_deref()
                .and_then(|e| HeaderValue::from_str(e).ok())
            {
                req.headers_mut().insert(IF_NONE_MATCH, v);
            } else if let Some(v) = c
                .last_modified
                .as_deref()
                .and_then(|e| HeaderValue::from_str(e).ok())
            {
                req.headers_mut().insert(IF_MODIFIED_SINCE, v);
            }
        }

        let slot = self.slot(&call.provider);
        let _permit = slot
            .acquire()
            .await
            .map_err(|e| GatewayError::Other(e.into()))?;
        self.limiter.record(&call.provider, resource);
        let resp = match self.client.execute(req).await {
            Ok(r) => r,
            Err(e) => {
                let err = GatewayError::Network {
                    provider: call.provider.clone(),
                    message: e.to_string(),
                };
                return self.stale_or(&call, &audit_url, cached, started, err);
            }
        };

        let now = Utc::now();
        let status = resp.status();
        let headers = resp.headers().clone();
        let (_, remaining, reset_at, _) = parse_limits(&headers, now);
        let limited = status == StatusCode::TOO_MANY_REQUESTS
            || (status == StatusCode::FORBIDDEN
                && (remaining == Some(0) || headers.contains_key(RETRY_AFTER)));
        let blocked_until = limited.then(|| {
            retry_after(&headers, now)
                .or(reset_at)
                .unwrap_or(now + chrono::Duration::seconds(60))
        });
        self.record_limits(&call.provider, resource, &headers, now, blocked_until);

        if let Some(until) = blocked_until {
            let err = GatewayError::RateLimited {
                provider: call.provider.clone(),
                resource: resource.into(),
                until,
            };
            return self.stale_or(&call, &audit_url, cached, started, err);
        }

        let ttl = call
            .cache_ttl
            .map(|d| chrono::Duration::from_std(d).unwrap_or_default())
            .unwrap_or_default();
        if status == StatusCode::NOT_MODIFIED
            && let (Some(k), Some(c)) = (&key, cached)
        {
            self.store.cache_touch(k, now, now + ttl)?;
            self.audit(
                &call,
                &audit_url,
                Outcome::Revalidated,
                Some(304),
                started,
                None,
            );
            return Ok(Response {
                status: c.status,
                body: c.body,
                outcome: Outcome::Revalidated,
                headers,
            });
        }

        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            let err = GatewayError::Http {
                provider: call.provider.clone(),
                status: status.as_u16(),
                body: body.chars().take(500).collect(),
            };
            self.audit(
                &call,
                &audit_url,
                Outcome::Error,
                Some(status.as_u16()),
                started,
                Some(err.to_string()),
            );
            return Err(err);
        }

        if let Some(k) = &key {
            let text = |h: HeaderName| {
                headers
                    .get(h)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string)
            };
            let entry = CachedResponse {
                status: status.as_u16(),
                body: body.clone(),
                etag: text(ETAG),
                last_modified: text(LAST_MODIFIED),
                fetched_at: now,
                expires_at: now + ttl,
            };
            self.store
                .cache_put(k, &call.provider, &audit_url, &entry)?;
        }
        self.audit(
            &call,
            &audit_url,
            Outcome::Network,
            Some(status.as_u16()),
            started,
            None,
        );
        Ok(Response {
            status: status.as_u16(),
            body,
            outcome: Outcome::Network,
            headers,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::AuditQuery;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A tiny HTTP server that replays canned responses in order.
    fn serve(responses: Vec<&'static str>) -> (String, Arc<AtomicUsize>, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = format!("http://{}", listener.local_addr().unwrap());
        let hits = Arc::new(AtomicUsize::new(0));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (h, s) = (hits.clone(), seen.clone());
        std::thread::spawn(move || {
            for (stream, resp) in listener.incoming().zip(responses) {
                let mut stream = stream.unwrap();
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap();
                s.lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&buf[..n]).to_lowercase());
                h.fetch_add(1, Ordering::SeqCst);
                stream.write_all(resp.as_bytes()).unwrap();
            }
        });
        (addr, hits, seen)
    }

    const OK: &str = "HTTP/1.1 200 OK\r\nETag: \"v1\"\r\nx-ratelimit-limit: 5000\r\nx-ratelimit-remaining: 4999\r\nx-ratelimit-reset: 4102444800\r\ncontent-length: 11\r\nconnection: close\r\n\r\n{\"pr\":\"ok\"}";
    const NOT_MODIFIED: &str =
        "HTTP/1.1 304 Not Modified\r\nx-ratelimit-remaining: 4999\r\nconnection: close\r\n\r\n";
    const LIMITED: &str = "HTTP/1.1 429 Too Many Requests\r\nretry-after: 120\r\ncontent-length: 0\r\nconnection: close\r\n\r\n";

    #[tokio::test]
    async fn caches_revalidates_and_respects_rate_limits() {
        let (base, hits, seen) = serve(vec![OK, NOT_MODIFIED, LIMITED]);
        let store = Arc::new(Store::in_memory().unwrap());
        let gw = Gateway::new(store.clone());
        let call = |ttl: u64| {
            Call::get(
                "github",
                "github",
                "pr preview",
                format!("{base}/repos/a/b/pulls/1"),
            )
            .cache(Duration::from_millis(ttl))
        };

        // Network, then a fresh cache hit with no request.
        let r = gw.send(call(60_000)).await.unwrap();
        assert_eq!(
            (r.outcome, r.body.as_str()),
            (Outcome::Network, "{\"pr\":\"ok\"}")
        );
        assert_eq!(
            gw.send(call(60_000)).await.unwrap().outcome,
            Outcome::CacheHit
        );
        assert_eq!(hits.load(Ordering::SeqCst), 1);

        // Expired: revalidates with the ETag and gets a 304.
        let other = Call::get(
            "github",
            "github",
            "pr preview",
            format!("{base}/repos/a/b/pulls/1"),
        )
        .cache(Duration::ZERO);
        let r = gw.send(other.clone()).await.unwrap();
        assert_eq!(r.outcome, Outcome::Revalidated);
        assert!(seen.lock().unwrap()[1].contains("if-none-match: \"v1\""));

        // 429: served stale, and the next call is refused locally without a request.
        assert_eq!(
            gw.send(other.clone()).await.unwrap().outcome,
            Outcome::Stale
        );
        assert_eq!(hits.load(Ordering::SeqCst), 3);
        let uncached = Call::get(
            "github",
            "github",
            "list repos",
            format!("{base}/user/repos"),
        );
        assert!(matches!(
            gw.send(uncached).await,
            Err(GatewayError::RateLimited { .. })
        ));
        assert_eq!(
            hits.load(Ordering::SeqCst),
            3,
            "blocked calls never leave the machine"
        );
        assert!(gw.rate_limits()[0].blocked_until.is_some());

        let outcomes: Vec<String> = store
            .audit_list(&AuditQuery::default())
            .unwrap()
            .into_iter()
            .rev()
            .map(|a| a.outcome)
            .collect();
        assert_eq!(
            outcomes,
            [
                "network",
                "cache_hit",
                "revalidated",
                "stale",
                "rate_limited"
            ]
        );
        let usage = store
            .usage_since(Utc::now() - chrono::Duration::hours(1))
            .unwrap();
        let preview = usage.iter().find(|u| u.purpose == "pr preview").unwrap();
        assert_eq!(
            (preview.total, preview.network, preview.cache_hits),
            (4, 1, 1)
        );
    }

    #[test]
    fn redacts_secret_query_values() {
        let u = reqwest::Url::parse("https://x.test/a?access_token=abc&page=2").unwrap();
        assert_eq!(redact(&u), "https://x.test/a?access_token=***&page=2");
    }
}
