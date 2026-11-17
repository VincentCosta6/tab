//! Leases: who is working on something, and until when.
//!
//! A lease is how two devices (or two people in an org) avoid doing the same
//! work: a runner claims a workflow step, a person claims a queue item. The
//! hub stores leases; these rules decide what a claim does, so every storage
//! backend behaves the same.
//!
//! - A claim on a free or expired key is granted with a new fencing token.
//! - The holder claiming again renews it (same token, later expiry).
//! - Anyone else is refused until it expires or is released.
//!
//! The fencing token only goes up for a key. A holder whose lease expired and
//! was taken by someone else still has its old token, so the hub can refuse
//! its late writes: a write can name a [`Fence`] (a lease key and the token
//! it was made under), and the hub applies it only while [`check_fence`]
//! passes, in the same transaction as the write.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

/// Shortest lease a claim may ask for.
pub const MIN_TTL: Duration = Duration::seconds(5);
/// Longest lease a claim may ask for. Longer work renews.
pub const MAX_TTL: Duration = Duration::hours(1);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Lease {
    /// What's leased, such as `item:<id>` or `step:<run>/<step>`.
    pub key: String,
    /// Who holds it: a device or person id.
    pub holder: String,
    /// Goes up every time the key changes hands.
    pub token: u64,
    pub expires_at: DateTime<Utc>,
}

impl Lease {
    pub fn is_live(&self, now: DateTime<Utc>) -> bool {
        now < self.expires_at
    }
}

/// What a claim did.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Claim {
    /// The key was free (or its lease had expired). Store this lease.
    Granted { lease: Lease },
    /// The holder already had it. Store this lease (same token).
    Renewed { lease: Lease },
    /// Someone else holds it.
    Held { lease: Lease },
}

impl Claim {
    pub fn lease(&self) -> &Lease {
        match self {
            Claim::Granted { lease } | Claim::Renewed { lease } | Claim::Held { lease } => lease,
        }
    }
    /// Whether the claimant now holds the lease.
    pub fn won(&self) -> bool {
        !matches!(self, Claim::Held { .. })
    }
}

/// Clamps a requested lease length to [`MIN_TTL`]..=[`MAX_TTL`].
pub fn clamp_ttl(ttl: Duration) -> Duration {
    ttl.clamp(MIN_TTL, MAX_TTL)
}

/// Decides a claim on `key` by `holder`, given the lease stored for that key
/// (if any) and the last token handed out for it.
pub fn claim(
    current: Option<&Lease>,
    last_token: u64,
    key: &str,
    holder: &str,
    ttl: Duration,
    now: DateTime<Utc>,
) -> Claim {
    let expires_at = now + clamp_ttl(ttl);
    match current {
        Some(l) if l.is_live(now) && l.holder == holder => Claim::Renewed {
            lease: Lease {
                expires_at,
                ..l.clone()
            },
        },
        Some(l) if l.is_live(now) => Claim::Held { lease: l.clone() },
        _ => Claim::Granted {
            lease: Lease {
                key: key.to_string(),
                holder: holder.to_string(),
                token: last_token.max(current.map_or(0, |l| l.token)) + 1,
                expires_at,
            },
        },
    }
}

/// Whether `holder` may release the stored lease. Releasing a lease you
/// don't hold (or that already expired) does nothing.
pub fn may_release(current: Option<&Lease>, holder: &str, now: DateTime<Utc>) -> bool {
    current.is_some_and(|l| l.holder == holder && l.is_live(now))
}

/// Whether a write fenced with `token` on `key` is still allowed: its lease
/// must be the live one.
pub fn fence_ok(current: Option<&Lease>, token: u64, now: DateTime<Utc>) -> bool {
    current.is_some_and(|l| l.token == token && l.is_live(now))
}

/// A write's claim to be made under a lease: "I hold `key` with `token`".
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Fence {
    /// The lease key, such as `job:<id>`.
    pub key: String,
    /// The fencing token the writer was granted for it.
    pub token: u64,
}

impl Fence {
    pub fn new(key: impl Into<String>, token: u64) -> Self {
        Self {
            key: key.into(),
            token,
        }
    }
}

/// Why a fenced write was refused: the lease it names isn't held by the
/// writer with that token at the time of the write (it expired, was
/// released, or someone else took it over). Not worth retrying: someone
/// else owns the work now.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FenceError {
    pub key: String,
    pub token: u64,
}

impl std::fmt::Display for FenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "lease {} isn't held with token {} any more",
            self.key, self.token
        )
    }
}

impl std::error::Error for FenceError {}

/// Checks a fenced write by `writer` against the lease stored for
/// `fence.key`: it passes only if that lease is live at `now`, held by
/// `writer`, with exactly `fence.token`.
pub fn check_fence(
    current: Option<&Lease>,
    fence: &Fence,
    writer: &str,
    now: DateTime<Utc>,
) -> Result<(), FenceError> {
    if current.is_some_and(|l| l.holder == writer) && fence_ok(current, fence.token, now) {
        return Ok(());
    }
    Err(FenceError {
        key: fence.key.clone(),
        token: fence.token,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000 + s, 0).unwrap()
    }

    #[test]
    fn grant_renew_refuse_take_over() {
        let ttl = Duration::seconds(30);
        let a = claim(None, 0, "item:1", "mac", ttl, t(0));
        let Claim::Granted { lease } = &a else {
            panic!("{a:?}")
        };
        assert_eq!(lease.token, 1);
        assert_eq!(lease.expires_at, t(30));

        let r = claim(Some(lease), 1, "item:1", "mac", ttl, t(10));
        assert!(
            matches!(&r, Claim::Renewed { lease } if lease.token == 1 && lease.expires_at == t(40))
        );

        let h = claim(Some(r.lease()), 1, "item:1", "phone", ttl, t(20));
        assert!(!h.won());
        assert_eq!(h.lease().holder, "mac");

        // After it lapses, someone else gets it with a higher token, and the
        // old holder's token no longer passes the fence.
        let g = claim(Some(r.lease()), 1, "item:1", "phone", ttl, t(41));
        assert!(
            matches!(&g, Claim::Granted { lease } if lease.token == 2 && lease.holder == "phone")
        );
        assert!(!fence_ok(Some(g.lease()), 1, t(42)));
        assert!(fence_ok(Some(g.lease()), 2, t(42)));
    }

    #[test]
    fn tokens_never_go_back_after_release() {
        // Released keys have no stored lease, but the last token is kept.
        let g = claim(None, 7, "k", "a", Duration::seconds(10), t(0));
        assert_eq!(g.lease().token, 8);
    }

    #[test]
    fn a_fence_passes_only_for_the_live_holder_with_its_token() {
        let ttl = Duration::seconds(30);
        let g = claim(None, 0, "job:1", "mac", ttl, t(0));
        let f = Fence::new("job:1", 1);
        assert_eq!(check_fence(Some(g.lease()), &f, "mac", t(29)), Ok(()));
        // Expired, even if nobody took it yet.
        assert!(check_fence(Some(g.lease()), &f, "mac", t(30)).is_err());
        // Another writer, with the right token.
        assert!(check_fence(Some(g.lease()), &f, "phone", t(1)).is_err());
        // An older or newer token.
        assert!(check_fence(Some(g.lease()), &Fence::new("job:1", 2), "mac", t(1)).is_err());
        // No lease at all (released, or never claimed).
        let e = check_fence(None, &f, "mac", t(1)).unwrap_err();
        assert_eq!(
            e.to_string(),
            "lease job:1 isn't held with token 1 any more"
        );
        // Taken over after it lapsed: the old token fails, the new one passes.
        let h = claim(Some(g.lease()), 1, "job:1", "phone", ttl, t(31));
        assert!(check_fence(Some(h.lease()), &f, "mac", t(32)).is_err());
        assert!(check_fence(Some(h.lease()), &Fence::new("job:1", 2), "phone", t(32)).is_ok());
    }

    #[test]
    fn ttl_is_clamped_and_release_needs_the_holder() {
        let g = claim(None, 0, "k", "a", Duration::days(3), t(0));
        assert_eq!(g.lease().expires_at, t(0) + MAX_TTL);
        assert!(may_release(Some(g.lease()), "a", t(1)));
        assert!(!may_release(Some(g.lease()), "b", t(1)));
        assert!(!may_release(Some(g.lease()), "a", t(0) + MAX_TTL));
    }
}
