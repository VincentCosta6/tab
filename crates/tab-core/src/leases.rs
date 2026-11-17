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
    /// Sends the changes this side has queued for the hub now, and waits
    /// for the hub's answer. A runner calls it after a write fenced by a
    /// lease ([`crate::store::Store::job_put_fenced`]) and before releasing
    /// that lease, since the hub checks the fence when the write arrives.
    fn flush_blocking(&self) -> anyhow::Result<()> {
        Ok(())
    }
}

/// The hub said no to a request: its error code and message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused {
    /// A `tab_protocol::pb::error::Code`.
    pub code: i32,
    /// Including which hub said it ("hub: ...", "cloud hub: ...").
    pub message: String,
}

impl Refused {
    pub fn new(hub: &str, e: &tab_protocol::pb::Error) -> Self {
        Self {
            code: e.code,
            message: format!("{hub}: {}", e.message),
        }
    }
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Refused {}

/// Whether the hub refused a write because its fence failed: the lease it
/// was made under isn't this side's any more. Someone else owns that work
/// now, so the write is dropped rather than tried again.
pub fn lost_lease(e: &anyhow::Error) -> bool {
    e.downcast_ref::<Refused>()
        .is_some_and(|r| r.code == tab_protocol::pb::error::Code::Fenced as i32)
}

/// [`Leases::flush_blocking`] on a blocking thread.
pub async fn flush(leases: &Arc<dyn Leases>) -> anyhow::Result<()> {
    let l = leases.clone();
    tokio::task::spawn_blocking(move || l.flush_blocking()).await?
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

/// A lease's holder, fencing token and expiry.
type Held = (String, u64, DateTime<Utc>);

/// Leases kept in memory, for a single runner with no hub (tests, and an
/// engine started without one): every claim by the same holder wins.
#[derive(Default)]
pub struct InMemory {
    held: std::sync::Mutex<std::collections::HashMap<String, Held>>,
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
        assert_eq!(
            l.claim_blocking("k", Duration::seconds(30)).unwrap().token,
            a.token
        );
        let b = l.claim_as("b", "k", Duration::seconds(30));
        assert!(!b.won);
        assert_eq!(b.holder, "a");
        l.release_blocking("k").unwrap();
        let b = l.claim_as("b", "k", Duration::seconds(30));
        assert!(b.won);
        assert!(b.token > a.token);
    }
}
