//! Conversions between the wire types in [`crate::pb`] and `tab-domain`.
//!
//! Domain → wire can't fail. Wire → domain can, since anything can arrive
//! on a socket: a missing field or a body that isn't JSON is a
//! [`ConvertError`], which the hub answers as a bad request.

use chrono::{DateTime, Utc};
use prost_types::Timestamp;
use tab_domain::lease::{Claim, Lease};
use tab_domain::ops::{Applied, Edge, Op, Record, SearchHit};

use crate::pb;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConvertError(pub String);

impl std::fmt::Display for ConvertError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ConvertError {}

fn missing(what: &str) -> ConvertError {
    ConvertError(format!("{what} is missing"))
}

pub fn timestamp(t: DateTime<Utc>) -> Timestamp {
    Timestamp {
        seconds: t.timestamp(),
        nanos: t.timestamp_subsec_nanos() as i32,
    }
}

pub fn from_timestamp(t: Option<Timestamp>, what: &str) -> Result<DateTime<Utc>, ConvertError> {
    let t = t.ok_or_else(|| missing(what))?;
    u32::try_from(t.nanos)
        .ok()
        .and_then(|n| DateTime::from_timestamp(t.seconds, n))
        .ok_or_else(|| ConvertError(format!("{what} is out of range")))
}

fn body_bytes(v: &serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(v).unwrap_or_default()
}

fn body_json(b: &[u8]) -> Result<serde_json::Value, ConvertError> {
    serde_json::from_slice(b).map_err(|e| ConvertError(format!("body isn't JSON: {e}")))
}

impl From<&Op> for pb::Op {
    fn from(op: &Op) -> Self {
        use pb::op::Op as W;
        let op = match op {
            Op::Put { kind, id, body } => W::Put(pb::Put {
                kind: kind.clone(),
                id: id.clone(),
                body_json: body_bytes(body),
            }),
            Op::Delete { kind, id } => W::Delete(pb::Delete {
                kind: kind.clone(),
                id: id.clone(),
            }),
            Op::Link { from, to, rel } => W::Link(pb::Link {
                from: from.clone(),
                to: to.clone(),
                rel: rel.clone(),
            }),
            Op::Unlink { from, to, rel } => W::Unlink(pb::Unlink {
                from: from.clone(),
                to: to.clone(),
                rel: rel.clone(),
            }),
        };
        pb::Op { op: Some(op) }
    }
}

impl TryFrom<pb::Op> for Op {
    type Error = ConvertError;
    fn try_from(op: pb::Op) -> Result<Self, ConvertError> {
        use pb::op::Op as W;
        Ok(match op.op.ok_or_else(|| missing("op"))? {
            W::Put(p) => Op::Put {
                kind: p.kind,
                id: p.id,
                body: body_json(&p.body_json)?,
            },
            W::Delete(d) => Op::Delete {
                kind: d.kind,
                id: d.id,
            },
            W::Link(l) => Op::Link {
                from: l.from,
                to: l.to,
                rel: l.rel,
            },
            W::Unlink(u) => Op::Unlink {
                from: u.from,
                to: u.to,
                rel: u.rel,
            },
        })
    }
}

impl From<&Applied> for pb::AppliedOp {
    fn from(a: &Applied) -> Self {
        pb::AppliedOp {
            seq: a.seq,
            op: Some((&a.op).into()),
            by: a.by.clone(),
            at: Some(timestamp(a.at)),
        }
    }
}

impl TryFrom<pb::AppliedOp> for Applied {
    type Error = ConvertError;
    fn try_from(a: pb::AppliedOp) -> Result<Self, ConvertError> {
        Ok(Applied {
            seq: a.seq,
            op: a.op.ok_or_else(|| missing("op"))?.try_into()?,
            by: a.by,
            at: from_timestamp(a.at, "at")?,
        })
    }
}

impl From<&Record> for pb::Record {
    fn from(r: &Record) -> Self {
        pb::Record {
            kind: r.kind.clone(),
            id: r.id.clone(),
            body_json: body_bytes(&r.body),
            seq: r.seq,
            updated_at: Some(timestamp(r.updated_at)),
        }
    }
}

impl TryFrom<pb::Record> for Record {
    type Error = ConvertError;
    fn try_from(r: pb::Record) -> Result<Self, ConvertError> {
        Ok(Record {
            kind: r.kind,
            id: r.id,
            body: body_json(&r.body_json)?,
            seq: r.seq,
            updated_at: from_timestamp(r.updated_at, "updated_at")?,
        })
    }
}

impl From<&Edge> for pb::Edge {
    fn from(e: &Edge) -> Self {
        pb::Edge {
            from: e.from.clone(),
            to: e.to.clone(),
            rel: e.rel.clone(),
        }
    }
}

impl From<pb::Edge> for Edge {
    fn from(e: pb::Edge) -> Self {
        Edge {
            from: e.from,
            to: e.to,
            rel: e.rel,
        }
    }
}

impl From<&Lease> for pb::Lease {
    fn from(l: &Lease) -> Self {
        pb::Lease {
            key: l.key.clone(),
            holder: l.holder.clone(),
            token: l.token,
            expires_at: Some(timestamp(l.expires_at)),
        }
    }
}

impl TryFrom<pb::Lease> for Lease {
    type Error = ConvertError;
    fn try_from(l: pb::Lease) -> Result<Self, ConvertError> {
        Ok(Lease {
            key: l.key,
            holder: l.holder,
            token: l.token,
            expires_at: from_timestamp(l.expires_at, "expires_at")?,
        })
    }
}

impl From<&Claim> for pb::LeaseClaim {
    fn from(c: &Claim) -> Self {
        use pb::lease_claim::Outcome;
        let outcome = match c {
            Claim::Granted { .. } => Outcome::Granted,
            Claim::Renewed { .. } => Outcome::Renewed,
            Claim::Held { .. } => Outcome::Held,
        };
        pb::LeaseClaim {
            outcome: outcome as i32,
            lease: Some(c.lease().into()),
        }
    }
}

impl TryFrom<pb::LeaseClaim> for Claim {
    type Error = ConvertError;
    fn try_from(c: pb::LeaseClaim) -> Result<Self, ConvertError> {
        use pb::lease_claim::Outcome;
        let lease = c.lease.ok_or_else(|| missing("lease"))?.try_into()?;
        match Outcome::try_from(c.outcome) {
            Ok(Outcome::Granted) => Ok(Claim::Granted { lease }),
            Ok(Outcome::Renewed) => Ok(Claim::Renewed { lease }),
            Ok(Outcome::Held) => Ok(Claim::Held { lease }),
            _ => Err(ConvertError(format!("unknown lease outcome {}", c.outcome))),
        }
    }
}

impl From<&SearchHit> for pb::SearchHit {
    fn from(h: &SearchHit) -> Self {
        pb::SearchHit {
            kind: h.kind.clone(),
            id: h.id.clone(),
            seq: h.seq,
        }
    }
}

impl From<pb::SearchHit> for SearchHit {
    fn from(h: pb::SearchHit) -> Self {
        SearchHit {
            kind: h.kind,
            id: h.id,
            seq: h.seq,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn t() -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000, 123_456_789).unwrap()
    }

    #[test]
    fn domain_values_survive_the_wire() {
        let ops = [
            Op::Put {
                kind: "item".into(),
                id: "1".into(),
                body: json!({"title": "Fix login", "n": 3, "tags": ["a"]}),
            },
            Op::Delete {
                kind: "item".into(),
                id: "2".into(),
            },
            Op::Link {
                from: "item:1".into(),
                to: "project:tab".into(),
                rel: "in".into(),
            },
            Op::Unlink {
                from: "a".into(),
                to: "b".into(),
                rel: "c".into(),
            },
        ];
        for (i, op) in ops.into_iter().enumerate() {
            let a = Applied {
                seq: i as u64 + 1,
                op,
                by: "mac".into(),
                at: t(),
            };
            let wire = crate::encode(&pb::AppliedOp::from(&a));
            let back: Applied = <pb::AppliedOp as prost::Message>::decode(&wire[..])
                .unwrap()
                .try_into()
                .unwrap();
            assert_eq!(back, a);
        }
        let claim = Claim::Held {
            lease: Lease {
                key: "item:1".into(),
                holder: "phone".into(),
                token: 4,
                expires_at: t(),
            },
        };
        assert_eq!(
            Claim::try_from(pb::LeaseClaim::from(&claim)).unwrap(),
            claim
        );
    }

    #[test]
    fn bad_wire_values_are_errors() {
        let put = pb::Op {
            op: Some(pb::op::Op::Put(pb::Put {
                kind: "item".into(),
                id: "1".into(),
                body_json: b"{not json".to_vec(),
            })),
        };
        assert!(Op::try_from(put).is_err());
        assert!(Op::try_from(pb::Op { op: None }).is_err());
        assert!(from_timestamp(None, "at").is_err());
        assert!(
            from_timestamp(
                Some(Timestamp {
                    seconds: 0,
                    nanos: -1
                }),
                "at"
            )
            .is_err()
        );
    }
}
