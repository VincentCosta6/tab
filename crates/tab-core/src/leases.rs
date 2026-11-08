//! Leases from the app's side: asking the hub "may I do this?".
//!
//! The hub grants each lease key to one holder at a time, with a fencing
//! token (see [`tab_domain::lease`]). The engine claims one before it runs
//! a workflow for a trigger, and the job runner before it works a job, so
//! two runners on one hub never do the same work. [`crate::local_hub::LocalHub`]
//! is the real implementation: its own hub, or the cloud hub once moved.
//!
//! Calls block (a cloud hub answers over its socket); call them from a
//! blocking thread, as [`claim`] and [`release`] do.

use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};

/// What a claim got.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claimed {
    /// Whether the claimant holds the lease now.
    pub won: bool,
    /// The fencing token (the holder's, if lost).
    pub token: u64,
    pub holder: String,
    pub expires_at: DateTime<Utc>,
}

pub trait Leases: Send + Sync + 'static {
    /// The holder name this side claims as: its device id on the hub.
    fn holder(&self) -> String;
    fn claim_blocking(&self, key: &str, ttl: Duration) -> anyhow::Result<Claimed>;
    fn release_blocking(&self, key: &str) -> anyhow::Result<()>;
}

/// Claims `key` for `ttl` on a blocking thread.
pub async fn claim(leases: &Arc<dyn Leases>, key: &str, ttl: Duration) -> anyhow::Result<Claimed> {
    let (l, key) = (leases.clone(), key.to_string());
    tokio::task::spawn_blocking(move || l.claim_blocking(&key, ttl)).await?
}

/// Releases `key` on a blocking thread.
pub async fn release(leases: &Arc<dyn Leases>, key: &str) -> anyhow::Result<()> {
    let (l, key) = (leases.clone(), key.to_string());
    tokio::task::spawn_blocking(move || l.release_blocking(&key)).await?
}

/// Leases kept in memory, for a single runner with no hub (tests, and an
/// engine started without one): every claim by the same holder wins.
#[derive(Default)]
pub struct InMemory {
    held: std::sync::Mutex<std::collections::HashMap<String, (String, u64, DateTime<Utc>)>>,
    holder: String,
}

impl InMemory {
    pub fn new(holder: &str) -> Arc<Self> {
        Arc::new(Self {
            held: Default::default(),
            holder: holder.into(),
        })
    }

    /// Claims as someone else, for tests of two runners.
    pub fn claim_as(&self, holder: &str, key: &str, ttl: Duration) -> Claimed {
        let now = Utc::now();
        let mut held = self.held.lock().unwrap();
        let last = held.get(key).map(|(_, t, _)| *t).unwrap_or(0);
        match held.get(key) {
            Some((h, t, exp)) if *exp > now && h != holder => Claimed {
                won: false,
                token: *t,
                holder: h.clone(),
                expires_at: *exp,
            },
            Some((h, t, _)) if h == holder => {
                let exp = now + ttl;
                let t = *t;
                held.insert(key.into(), (holder.into(), t, exp));
                Claimed {
                    won: true,
                    token: t,
                    holder: holder.into(),
                    expires_at: exp,
                }
            }
            _ => {
                let exp = now + ttl;
                held.insert(key.into(), (holder.into(), last + 1, exp));
                Claimed {
                    won: true,
                    token: last + 1,
                    holder: holder.into(),
                    expires_at: exp,
                }
            }
        }
    }
}

impl Leases for InMemory {
    fn holder(&self) -> String {
        self.holder.clone()
    }
    fn claim_blocking(&self, key: &str, ttl: Duration) -> anyhow::Result<Claimed> {
        Ok(self.claim_as(&self.holder, key, ttl))
    }
    fn release_blocking(&self, key: &str) -> anyhow::Result<()> {
        let mut held = self.held.lock().unwrap();
        if held.get(key).is_some_and(|(h, _, _)| h == &self.holder) {
            // Keep the token so the next holder's is higher.
            if let Some(e) = held.get_mut(key) {
                e.2 = Utc::now();
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_holder_at_a_time_and_tokens_go_up() {
        let l = InMemory::new("a");
        let a = l.claim_blocking("k", Duration::seconds(30)).unwrap();
        assert!(a.won);
        assert_eq!(l.claim_blocking("k", Duration::seconds(30)).unwrap().token, a.token);
        let b = l.claim_as("b", "k", Duration::seconds(30));
        assert!(!b.won);
        assert_eq!(b.holder, "a");
        l.release_blocking("k").unwrap();
        let b = l.claim_as("b", "k", Duration::seconds(30));
        assert!(b.won);
        assert!(b.token > a.token);
    }
}
