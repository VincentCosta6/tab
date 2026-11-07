//! An in-memory [`HubStore`]. It's the reference the other backends are
//! checked against, so it favors being obviously right over being fast.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Duration, Utc};
use tab_domain::lease::{self, Claim, Lease};
use tab_domain::ops::{self, Applied, Edge, Op, Record, SearchHit, Seq};

use crate::{HubStore, Result, StoreStats};

#[derive(Debug, Default, Clone)]
pub struct MemStore {
    log: Vec<Applied>,
    records: BTreeMap<(String, String), Record>,
    edges: BTreeSet<Edge>,
    leases: BTreeMap<String, Lease>,
    /// Last fencing token handed out per key, kept after release.
    tokens: BTreeMap<String, u64>,
}

impl MemStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl HubStore for MemStore {
    fn head(&self) -> Result<Seq> {
        Ok(self.log.last().map_or(0, |a| a.seq))
    }

    fn apply(&mut self, by: &str, batch: &[Op], now: DateTime<Utc>) -> Result<Vec<Applied>> {
        ops::validate(batch)?;
        let head = self.head()?;
        let mut out = Vec::with_capacity(batch.len());
        for (seq, op) in (head + 1..).zip(batch) {
            match op {
                Op::Put { kind, id, body } => {
                    self.records.insert(
                        (kind.clone(), id.clone()),
                        Record {
                            kind: kind.clone(),
                            id: id.clone(),
                            body: body.clone(),
                            seq,
                            updated_at: now,
                        },
                    );
                }
                Op::Delete { kind, id } => {
                    self.records.remove(&(kind.clone(), id.clone()));
                    let n = ops::node(kind, id);
                    self.edges.retain(|e| e.from != n && e.to != n);
                }
                Op::Link { from, to, rel } => {
                    self.edges.insert(Edge {
                        from: from.clone(),
                        to: to.clone(),
                        rel: rel.clone(),
                    });
                }
                Op::Unlink { from, to, rel } => {
                    self.edges.remove(&Edge {
                        from: from.clone(),
                        to: to.clone(),
                        rel: rel.clone(),
                    });
                }
            }
            out.push(Applied {
                seq,
                op: op.clone(),
                by: by.to_string(),
                at: now,
            });
        }
        self.log.extend(out.iter().cloned());
        Ok(out)
    }

    fn changes_since(&self, since: Seq, limit: usize) -> Result<Vec<Applied>> {
        Ok(self
            .log
            .iter()
            .filter(|a| a.seq > since)
            .take(limit)
            .cloned()
            .collect())
    }

    fn get(&self, kind: &str, id: &str) -> Result<Option<Record>> {
        Ok(self
            .records
            .get(&(kind.to_string(), id.to_string()))
            .cloned())
    }

    fn claim_lease(
        &mut self,
        key: &str,
        holder: &str,
        ttl: Duration,
        now: DateTime<Utc>,
    ) -> Result<Claim> {
        let last = self.tokens.get(key).copied().unwrap_or(0);
        let c = lease::claim(self.leases.get(key), last, key, holder, ttl, now);
        if c.won() {
            let l = c.lease().clone();
            self.tokens.insert(key.to_string(), l.token);
            self.leases.insert(key.to_string(), l);
        }
        Ok(c)
    }

    fn release_lease(&mut self, key: &str, holder: &str, now: DateTime<Utc>) -> Result<bool> {
        if lease::may_release(self.leases.get(key), holder, now) {
            self.leases.remove(key);
            return Ok(true);
        }
        Ok(false)
    }

    fn expire_leases(&mut self, now: DateTime<Utc>) -> Result<Vec<Lease>> {
        let gone: Vec<Lease> = self
            .leases
            .values()
            .filter(|l| !l.is_live(now))
            .cloned()
            .collect();
        for l in &gone {
            self.leases.remove(&l.key);
        }
        Ok(gone)
    }

    fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        let terms = ops::search_terms(query);
        if terms.is_empty() {
            return Ok(vec![]);
        }
        let mut hits: Vec<SearchHit> = self
            .records
            .values()
            .filter(|r| {
                let text = ops::search_text(&r.body).to_lowercase();
                terms.iter().all(|t| text.contains(t.as_str()))
            })
            .map(|r| SearchHit {
                kind: r.kind.clone(),
                id: r.id.clone(),
                seq: r.seq,
            })
            .collect();
        hits.sort_by_key(|h| std::cmp::Reverse(h.seq));
        hits.truncate(limit);
        Ok(hits)
    }

    fn graph_neighbors(&self, node: &str, limit: usize) -> Result<Vec<Edge>> {
        Ok(self
            .edges
            .iter()
            .filter(|e| e.from == node || e.to == node)
            .take(limit)
            .cloned()
            .collect())
    }

    fn stats(&self, now: DateTime<Utc>) -> Result<StoreStats> {
        let records: usize = self
            .records
            .values()
            .map(|r| r.kind.len() + r.id.len() + r.body.to_string().len())
            .sum();
        let edges: usize = self
            .edges
            .iter()
            .map(|e| e.from.len() + e.to.len() + e.rel.len())
            .sum();
        let log: usize = self.log.len() * std::mem::size_of::<Applied>();
        Ok(StoreStats {
            records: self.records.len() as u64,
            edges: self.edges.len() as u64,
            live_leases: self.leases.values().filter(|l| l.is_live(now)).count() as u64,
            bytes: (records + edges + log) as u64,
        })
    }

    fn blob_refs(&self) -> Result<BTreeSet<String>> {
        Ok(self
            .records
            .values()
            .flat_map(|r| tab_domain::blob::refs(&r.body))
            .collect())
    }
}
