//! `tab-hub` is the part of tab that owns work state: the queue's records,
//! the knowledge graph, leases, and the change log every device syncs from.
//!
//! The same [`Hub`] runs in two places, on whatever [`HubStore`] it's given:
//!
//! - **In the desktop app**, on local SQLite, for people running tab
//!   entirely on their own machine.
//! - **In the cloud**, one per person or team inside a Cloudflare Durable
//!   Object on its built-in SQLite (the `cloud/worker` crate, to come).
//!
//! It holds no sockets and reads no clock. Its host hands it decoded frames
//! with the current time and sends back what it returns: a reply to the
//! caller, and sometimes a push for every other connected device. That keeps
//! it building for Wasm and makes it easy to test.
//!
//! Moving between local and cloud is [`migrate::copy`]: replay one hub's log
//! into another.

pub mod migrate;

use chrono::{DateTime, Duration, Utc};
use tab_domain::ops::Op;
use tab_protocol::pb::{self, client_envelope::Body as Req, server_envelope::Body as Res};
use tab_protocol::{
    DEFAULT_CHANGES_PER_PAGE, DEFAULT_RESULTS, MAX_CHANGES_PER_PAGE, MAX_RESULTS, PROTOCOL_VERSION,
    ServerEnvelope,
};
pub use tab_store::HubStore;
use tab_store::StoreError;

/// What handling one request produced.
#[derive(Debug, Clone, PartialEq)]
pub struct Handled {
    /// Send to the device that asked.
    pub reply: ServerEnvelope,
    /// Send to every other connected device.
    pub push: Option<ServerEnvelope>,
}

pub struct Hub<S> {
    store: S,
}

fn page(asked: u32, default: u32, max: u32) -> usize {
    (if asked == 0 { default } else { asked.min(max) }) as usize
}

fn store_error(id: u64, e: StoreError) -> ServerEnvelope {
    let code = match e {
        StoreError::Invalid(_) => pb::error::Code::BadRequest,
        StoreError::Backend(_) => pb::error::Code::Internal,
    };
    ServerEnvelope::error(id, code, e.to_string())
}

impl<S: HubStore> Hub<S> {
    pub fn new(store: S) -> Self {
        Self { store }
    }

    pub fn store(&self) -> &S {
        &self.store
    }

    pub fn into_store(self) -> S {
        self.store
    }

    /// Handles one binary frame from `device`. Undecodable frames get an
    /// error reply.
    pub fn handle_frame(&mut self, device: &str, frame: &[u8], now: DateTime<Utc>) -> Handled {
        match tab_protocol::decode_client(frame) {
            Ok((id, req)) => self.handle(device, id, req, now),
            Err(reply) => Handled { reply, push: None },
        }
    }

    /// Handles one request from `device`.
    pub fn handle(&mut self, device: &str, id: u64, req: Req, now: DateTime<Utc>) -> Handled {
        match self.answer(device, id, req, now) {
            Ok(h) => h,
            Err(reply) => Handled { reply, push: None },
        }
    }

    fn answer(
        &mut self,
        device: &str,
        id: u64,
        req: Req,
        now: DateTime<Utc>,
    ) -> Result<Handled, ServerEnvelope> {
        let err = |e| store_error(id, e);
        let reply = |body| Handled {
            reply: ServerEnvelope::new(id, body),
            push: None,
        };
        Ok(match req {
            Req::Hello(_) => reply(Res::Welcome(pb::Welcome {
                protocol_version: PROTOCOL_VERSION,
                head: self.store.head().map_err(err)?,
            })),
            Req::Apply(a) => {
                let ops = a
                    .ops
                    .into_iter()
                    .map(Op::try_from)
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|e| {
                        ServerEnvelope::error(id, pb::error::Code::BadRequest, e.to_string())
                    })?;
                let applied = self.store.apply(device, &ops, now).map_err(err)?;
                let head = self.store.head().map_err(err)?;
                let push = (!applied.is_empty()).then(|| {
                    ServerEnvelope::new(
                        0,
                        Res::Changes(pb::Changes {
                            changes: applied.iter().map(Into::into).collect(),
                            head,
                            more: false,
                        }),
                    )
                });
                Handled {
                    reply: ServerEnvelope::new(
                        id,
                        Res::Applied(pb::Applied {
                            first: applied.first().map_or(0, |a| a.seq),
                            last: applied.last().map_or(0, |a| a.seq),
                        }),
                    ),
                    push,
                }
            }
            Req::GetChanges(g) => {
                let limit = page(g.limit, DEFAULT_CHANGES_PER_PAGE, MAX_CHANGES_PER_PAGE);
                // One extra tells us whether there's more.
                let mut changes = self.store.changes_since(g.since, limit + 1).map_err(err)?;
                let more = changes.len() > limit;
                changes.truncate(limit);
                reply(Res::Changes(pb::Changes {
                    changes: changes.iter().map(Into::into).collect(),
                    head: self.store.head().map_err(err)?,
                    more,
                }))
            }
            Req::GetRecord(g) => reply(Res::Record(pb::RecordReply {
                record: self
                    .store
                    .get(&g.kind, &g.id)
                    .map_err(err)?
                    .as_ref()
                    .map(Into::into),
            })),
            Req::ClaimLease(c) => {
                let claim = self
                    .store
                    .claim_lease(&c.key, device, Duration::seconds(c.ttl_secs.into()), now)
                    .map_err(err)?;
                reply(Res::LeaseClaim((&claim).into()))
            }
            Req::ReleaseLease(r) => reply(Res::Released(pb::Released {
                released: self.store.release_lease(&r.key, device, now).map_err(err)?,
            })),
            Req::Search(s) => {
                let hits = self
                    .store
                    .search(&s.query, page(s.limit, DEFAULT_RESULTS, MAX_RESULTS))
                    .map_err(err)?;
                reply(Res::SearchResults(pb::SearchResults {
                    hits: hits.iter().map(Into::into).collect(),
                }))
            }
            Req::GetNeighbors(n) => {
                let edges = self
                    .store
                    .graph_neighbors(&n.node, page(n.limit, DEFAULT_RESULTS, MAX_RESULTS))
                    .map_err(err)?;
                reply(Res::Neighbors(pb::Neighbors {
                    edges: edges.iter().map(Into::into).collect(),
                }))
            }
        })
    }

    /// Housekeeping the host runs on a timer (a Durable Object alarm, or a
    /// task in the desktop app): expires lapsed leases and returns a push
    /// telling devices, if any lapsed.
    pub fn tick(&mut self, now: DateTime<Utc>) -> Result<Option<ServerEnvelope>, StoreError> {
        let gone = self.store.expire_leases(now)?;
        Ok((!gone.is_empty()).then(|| {
            ServerEnvelope::new(
                0,
                Res::LeasesExpired(pb::LeasesExpired {
                    leases: gone.iter().map(Into::into).collect(),
                }),
            )
        }))
    }
}

#[cfg(test)]
mod tests;
