//! Client-side rate budgets, shared by every connector.
//!
//! Providers tell us where we stand through headers (handled by the gateway),
//! but by then a burst may already have spent the budget. So each connector
//! also *declares* its limits ([`RateLimitSpec`]) and the gateway keeps a
//! sliding window of what we actually sent. From both it computes
//! [`Pressure`]: how much headroom is left and whether we are bursting.
//!
//! Calls carry a [`Priority`]. Interactive calls (the person opened something)
//! go out unless the declared budget is fully spent. Background calls (the
//! preview refresher, polls) only go out with comfortable headroom and no
//! recent burst; otherwise they are deferred and the caller keeps its cache.
//! Background work opts in by running inside [`background`], so connectors
//! need no extra parameters.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A limit a connector knows its provider enforces.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RateLimitSpec {
    pub provider: String,
    /// Bucket name, matching what the gateway derives from the URL
    /// (`core`, `search`, `requests`, `default`).
    pub resource: String,
    /// Requests allowed per window.
    pub requests: u32,
    pub per_seconds: u32,
    /// More than this many requests in [`BURST_WINDOW`] counts as a burst
    /// and pauses background calls for [`COOL_DOWN`].
    pub burst: u32,
    #[serde(default)]
    pub note: String,
}

impl RateLimitSpec {
    pub fn new(
        provider: &str,
        resource: &str,
        requests: u32,
        per_seconds: u32,
        burst: u32,
    ) -> Self {
        Self {
            provider: provider.into(),
            resource: resource.into(),
            requests,
            per_seconds,
            burst,
            note: String::new(),
        }
    }
    pub fn note(mut self, note: &str) -> Self {
        self.note = note.into();
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    /// The person is waiting on this.
    Interactive,
    /// Nobody is waiting; only spend budget we can spare.
    Background,
}

tokio::task_local! {
    static PRIORITY: Priority;
}

/// Runs `f` with every gateway call inside it marked as background work.
pub async fn background<F: Future>(f: F) -> F::Output {
    PRIORITY.scope(Priority::Background, f).await
}

pub fn current_priority() -> Priority {
    PRIORITY.try_with(|p| *p).unwrap_or(Priority::Interactive)
}

pub const BURST_WINDOW: Duration = Duration::from_secs(10);
pub const COOL_DOWN: Duration = Duration::from_secs(30);
/// Background calls need at least this much of the budget left.
pub const BACKGROUND_HEADROOM: f32 = 0.3;
/// Used when nothing was declared for a bucket.
const DEFAULT_BURST: u32 = 20;

/// Where one bucket stands right now, for the UI and for admission.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Pressure {
    pub provider: String,
    pub resource: String,
    /// Requests we sent in the declared window.
    pub used: u32,
    /// Declared requests per window, if known.
    pub budget: Option<u32>,
    pub window_seconds: Option<u32>,
    /// What the provider last reported, if anything.
    pub remote_remaining: Option<u64>,
    pub remote_limit: Option<u64>,
    /// 0.0 (nothing left) to 1.0 (untouched): the lower of local and remote.
    pub headroom: f32,
    /// Requests in the last ten seconds.
    pub recent: u32,
    pub burst_threshold: u32,
    /// Background calls are paused until then after a burst.
    pub cooling_until: Option<DateTime<Utc>>,
    /// Background calls deferred so far.
    pub deferred: u64,
}

/// Why a call was not admitted.
#[derive(Debug, Clone, PartialEq)]
pub enum Refusal {
    /// The declared budget is spent; nothing goes out until `until`.
    Exhausted { until: DateTime<Utc> },
    /// Background call held back to leave room for interactive ones.
    Deferred {
        reason: String,
        until: DateTime<Utc>,
    },
}

#[derive(Default)]
struct Bucket {
    spec: Option<RateLimitSpec>,
    sent: VecDeque<Instant>,
    cooling_until: Option<Instant>,
    deferred: u64,
}

impl Bucket {
    fn window(&self) -> Duration {
        self.spec
            .as_ref()
            .map(|s| Duration::from_secs(s.per_seconds.max(1) as u64))
            .unwrap_or(Duration::from_secs(3600))
    }
    fn trim(&mut self, now: Instant) {
        let keep = self.window().max(BURST_WINDOW);
        while self
            .sent
            .front()
            .is_some_and(|t| now.duration_since(*t) > keep)
        {
            self.sent.pop_front();
        }
    }
    fn used(&self, now: Instant) -> u32 {
        let w = self.window();
        self.sent
            .iter()
            .filter(|t| now.duration_since(**t) <= w)
            .count() as u32
    }
    fn recent(&self, now: Instant) -> u32 {
        self.sent
            .iter()
            .filter(|t| now.duration_since(**t) <= BURST_WINDOW)
            .count() as u32
    }
    fn burst(&self) -> u32 {
        self.spec.as_ref().map(|s| s.burst).unwrap_or(DEFAULT_BURST)
    }
}

fn at(now: Instant, then: Instant) -> DateTime<Utc> {
    Utc::now() + chrono::Duration::from_std(then.saturating_duration_since(now)).unwrap_or_default()
}

/// Sliding-window usage per `(provider, resource)`.
#[derive(Default)]
pub struct Limiter {
    buckets: Mutex<HashMap<(String, String), Bucket>>,
}

impl Limiter {
    /// Declares limits. Replaces earlier declarations for the same buckets.
    pub fn declare(&self, specs: &[RateLimitSpec]) {
        let mut all = self.buckets.lock().unwrap();
        for s in specs {
            all.entry((s.provider.clone(), s.resource.clone()))
                .or_default()
                .spec = Some(s.clone());
        }
    }

    /// Decides whether a call may go out. `remote` is the provider's last
    /// reported `(remaining, limit)` for this bucket.
    pub fn admit(
        &self,
        provider: &str,
        resource: &str,
        priority: Priority,
        remote: Option<(u64, Option<u64>)>,
    ) -> Result<(), Refusal> {
        let now = Instant::now();
        let mut all = self.buckets.lock().unwrap();
        let b = all.entry((provider.into(), resource.into())).or_default();
        b.trim(now);
        let used = b.used(now);

        if let Some(spec) = &b.spec
            && used >= spec.requests
        {
            let oldest = b
                .sent
                .iter()
                .rev()
                .nth(spec.requests as usize - 1)
                .copied()
                .unwrap_or(now);
            return Err(Refusal::Exhausted {
                until: at(now, oldest + b.window()),
            });
        }
        if priority == Priority::Interactive {
            return Ok(());
        }

        if b.recent(now) > b.burst() {
            b.cooling_until = Some(now + COOL_DOWN);
        }
        if let Some(until) = b.cooling_until.filter(|u| *u > now) {
            b.deferred += 1;
            return Err(Refusal::Deferred {
                reason: "cooling down after a burst".into(),
                until: at(now, until),
            });
        }
        let headroom = headroom(b, used, remote);
        if headroom < BACKGROUND_HEADROOM {
            b.deferred += 1;
            return Err(Refusal::Deferred {
                reason: format!("only {:.0}% of the budget left", headroom * 100.0),
                until: at(now, now + Duration::from_secs(60)),
            });
        }
        Ok(())
    }

    /// Records a request that actually went over the network.
    pub fn record(&self, provider: &str, resource: &str) {
        let now = Instant::now();
        let mut all = self.buckets.lock().unwrap();
        let b = all.entry((provider.into(), resource.into())).or_default();
        b.sent.push_back(now);
        b.trim(now);
    }

    pub fn pressure(
        &self,
        remote: impl Fn(&str, &str) -> Option<(u64, Option<u64>)>,
    ) -> Vec<Pressure> {
        let now = Instant::now();
        let mut all = self.buckets.lock().unwrap();
        let mut v: Vec<Pressure> = all
            .iter_mut()
            .map(|((p, r), b)| {
                b.trim(now);
                let used = b.used(now);
                let rem = remote(p, r);
                Pressure {
                    provider: p.clone(),
                    resource: r.clone(),
                    used,
                    budget: b.spec.as_ref().map(|s| s.requests),
                    window_seconds: b.spec.as_ref().map(|s| s.per_seconds),
                    remote_remaining: rem.map(|r| r.0),
                    remote_limit: rem.and_then(|r| r.1),
                    headroom: headroom(b, used, rem),
                    recent: b.recent(now),
                    burst_threshold: b.burst(),
                    cooling_until: b.cooling_until.filter(|u| *u > now).map(|u| at(now, u)),
                    deferred: b.deferred,
                }
            })
            .collect();
        v.sort_by(|a, b| (&a.provider, &a.resource).cmp(&(&b.provider, &b.resource)));
        v
    }
}

fn headroom(b: &Bucket, used: u32, remote: Option<(u64, Option<u64>)>) -> f32 {
    let local = b
        .spec
        .as_ref()
        .map(|s| 1.0 - used as f32 / s.requests.max(1) as f32)
        .unwrap_or(1.0);
    let remote = match remote {
        Some((remaining, Some(limit))) if limit > 0 => remaining as f32 / limit as f32,
        _ => 1.0,
    };
    local.min(remote).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn background_defers_on_bursts_and_low_headroom_interactive_does_not() {
        let l = Limiter::default();
        l.declare(&[RateLimitSpec::new("slack", "default", 10, 60, 3)]);
        for _ in 0..4 {
            l.admit("slack", "default", Priority::Interactive, None)
                .unwrap();
            l.record("slack", "default");
        }
        // Four calls in ten seconds is a burst: background waits, interactive goes.
        assert!(matches!(
            l.admit("slack", "default", Priority::Background, None),
            Err(Refusal::Deferred { .. })
        ));
        l.admit("slack", "default", Priority::Interactive, None)
            .unwrap();
        for _ in 0..6 {
            l.record("slack", "default");
        }
        // Budget spent: even interactive calls stop.
        assert!(matches!(
            l.admit("slack", "default", Priority::Interactive, None),
            Err(Refusal::Exhausted { .. })
        ));
        let p = &l.pressure(|_, _| None)[0];
        assert_eq!((p.used, p.budget, p.headroom), (10, Some(10), 0.0));
        assert!(p.cooling_until.is_some() && p.deferred == 1);
    }

    #[test]
    fn remote_headroom_counts_too() {
        let l = Limiter::default();
        assert!(
            l.admit(
                "github",
                "core",
                Priority::Background,
                Some((4000, Some(5000)))
            )
            .is_ok()
        );
        assert!(matches!(
            l.admit(
                "github",
                "core",
                Priority::Background,
                Some((100, Some(5000)))
            ),
            Err(Refusal::Deferred { .. })
        ));
    }

    #[tokio::test]
    async fn background_scope_sets_priority() {
        assert_eq!(current_priority(), Priority::Interactive);
        assert_eq!(
            background(async { current_priority() }).await,
            Priority::Background
        );
    }
}
