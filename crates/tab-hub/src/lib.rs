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
//! It holds no sockets and reads no clock. Its host tells it when a
//! connection opens or closes ([`Hub::connect`], [`Hub::disconnect`]), hands
//! it each frame with the current time, and sends back what it returns: a
//! reply to the caller, and sometimes a push for every other connection.
//! That keeps it building for Wasm and makes it easy to test.
//!
//! Every connection says [`Hello`](pb::Hello) first, naming its device; the
//! hub refuses anything else until then. It keeps track of who is connected
//! and how far each one has synced, for the Hub page ([`Hub::status`]).
//!
//! Agent sessions are watched and steered from any device through the
//! [live channel](live): the device running a session publishes what it
//! streams, the hub relays it to watchers without storing it, and routes
//! steering back to the runner.
//!
//! Files live beside the records in a [`tab_store::blob::BlobStore`] the
//! host holds; [`blobs::collect`] deletes the ones nothing refers to.
//!
//! Moving between local and cloud is [`migrate::copy`]: replay one hub's log
//! into another.

pub mod blobs;
pub mod live;
pub mod migrate;

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Duration, Utc};
use tab_domain::ops::Op;
use tab_protocol::convert::timestamp;
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
    /// Send to these connections only (live events, steering).
    pub direct: Vec<(ConnId, ServerEnvelope)>,
}

impl Handled {
    fn reply(reply: ServerEnvelope) -> Self {
        Self {
            reply,
            push: None,
            direct: vec![],
        }
    }
}

/// The host's id for one connection (a socket, or the desktop app itself).
pub type ConnId = u64;

#[derive(Debug, Clone, PartialEq)]
struct Conn {
    device: Option<String>,
    client: String,
    address: String,
    connected_at: DateTime<Utc>,
    synced_to: u64,
    /// Sessions it wants live events for.
    watching: BTreeSet<String>,
    /// It's importing another hub's history (see [`pb::Import`]), and the
    /// log has nothing but what it sent.
    importing: bool,
}

pub struct Hub<S> {
    store: S,
    runs_on: String,
    started_at: DateTime<Utc>,
    conns: BTreeMap<ConnId, Conn>,
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
    /// A hub on `store`. `runs_on` is `"local"` or `"cloud"`, for status.
    pub fn new(store: S, runs_on: &str, now: DateTime<Utc>) -> Self {
        Self {
            store,
            runs_on: runs_on.to_string(),
            started_at: now,
            conns: BTreeMap::new(),
        }
    }

    /// A connection opened. `address` is for display only.
    pub fn connect(&mut self, conn: ConnId, address: &str, now: DateTime<Utc>) {
        self.conns.insert(
            conn,
            Conn {
                device: None,
                client: String::new(),
                address: address.to_string(),
                connected_at: now,
                synced_to: 0,
                watching: BTreeSet::new(),
                importing: false,
            },
        );
    }

    /// A connection closed. Returns what to tell the runners of sessions it
    /// was watching.
    pub fn disconnect(&mut self, conn: ConnId) -> Vec<(ConnId, ServerEnvelope)> {
        let Some(c) = self.conns.remove(&conn) else {
            return vec![];
        };
        c.watching
            .iter()
            .flat_map(|s| self.watched_notice(s))
            .collect()
    }

    /// Connections that have said hello: the ones the host sends pushes to.
    pub fn subscribers(&self) -> impl Iterator<Item = ConnId> + '_ {
        self.conns
            .iter()
            .filter(|(_, c)| c.device.is_some())
            .map(|(id, _)| *id)
    }

    /// What the hub holds and who's connected. The host adds what only it
    /// can measure (CPU, memory) to `resources`.
    pub fn status(&self, now: DateTime<Utc>) -> Result<pb::HubStatus, StoreError> {
        let stats = self.store.stats(now)?;
        Ok(pb::HubStatus {
            runs_on: self.runs_on.clone(),
            protocol_version: PROTOCOL_VERSION,
            started_at: Some(timestamp(self.started_at)),
            head: self.store.head()?,
            records: stats.records,
            edges: stats.edges,
            live_leases: stats.live_leases,
            storage_bytes: stats.bytes,
            connections: self
                .conns
                .iter()
                .map(|(id, c)| pb::Connection {
                    id: *id,
                    device: c.device.clone().unwrap_or_default(),
                    client: c.client.clone(),
                    address: c.address.clone(),
                    connected_at: Some(timestamp(c.connected_at)),
                    synced_to: c.synced_to,
                })
                .collect(),
            resources: None,
        })
    }

    pub fn store(&self) -> &S {
        &self.store
    }

    pub fn into_store(self) -> S {
        self.store
    }

    /// Handles one binary frame from connection `conn`. Undecodable frames
    /// get an error reply.
    pub fn handle_frame(&mut self, conn: ConnId, frame: &[u8], now: DateTime<Utc>) -> Handled {
        match tab_protocol::decode_client(frame) {
            Ok((id, req)) => self.handle(conn, id, req, now),
            Err(reply) => Handled::reply(reply),
        }
    }

    /// Handles one request from connection `conn`.
    pub fn handle(&mut self, conn: ConnId, id: u64, req: Req, now: DateTime<Utc>) -> Handled {
        match self.answer(conn, id, req, now) {
            Ok(h) => h,
            Err(reply) => Handled::reply(reply),
        }
    }

    // The error is the reply to send, so its size doesn't matter.
    #[allow(clippy::result_large_err)]
    fn answer(
        &mut self,
        conn: ConnId,
        id: u64,
        req: Req,
        now: DateTime<Utc>,
    ) -> Result<Handled, ServerEnvelope> {
        let err = |e| store_error(id, e);
        let reply = |body| Handled::reply(ServerEnvelope::new(id, body));
        let Some(c) = self.conns.get_mut(&conn) else {
            return Err(ServerEnvelope::error(
                id,
                pb::error::Code::BadRequest,
                "unknown connection",
            ));
        };
        if let Req::Hello(h) = &req {
            if h.device.is_empty() {
                return Err(ServerEnvelope::error(
                    id,
                    pb::error::Code::BadRequest,
                    "hello needs a device id",
                ));
            }
            c.device = Some(h.device.clone());
            c.client = h.client.clone();
            c.synced_to = h.since;
        }
        let Some(device) = c.device.clone() else {
            return Err(ServerEnvelope::error(
                id,
                pb::error::Code::BadRequest,
                "say hello first",
            ));
        };
        let device = device.as_str();
        Ok(match req {
            Req::Hello(_) => reply(Res::Welcome(pb::Welcome {
                protocol_version: PROTOCOL_VERSION,
                head: self.store.head().map_err(err)?,
            })),
            Req::GetHubStatus(_) => reply(Res::HubStatus(self.status(now).map_err(err)?)),
            Req::Apply(a) => {
                let ops = a
                    .ops
                    .into_iter()
                    .map(Op::try_from)
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|e| {
                        ServerEnvelope::error(id, pb::error::Code::BadRequest, e.to_string())
                    })?;
                let before = self.store.head().map_err(err)?;
                let applied = self.store.apply(device, &ops, now).map_err(err)?;
                let push = self.wrote(conn, before, &applied).map_err(err)?;
                Handled {
                    reply: ServerEnvelope::new(
                        id,
                        Res::Applied(pb::Applied {
                            first: applied.first().map_or(0, |a| a.seq),
                            last: applied.last().map_or(0, |a| a.seq),
                        }),
                    ),
                    push,
                    direct: vec![],
                }
            }
            Req::Import(im) => self.import(conn, id, im)?,
            Req::Watch(w) => self.watch(conn, id, w.sessions),
            Req::Publish(p) => self.publish(conn, device, id, p).map_err(|e| e.reply(id))?,
            Req::Steer(st) => self.steer(device, id, st).map_err(|e| e.reply(id))?,
            Req::GetChanges(g) => {
                let limit = page(g.limit, DEFAULT_CHANGES_PER_PAGE, MAX_CHANGES_PER_PAGE);
                // One extra tells us whether there's more.
                let mut changes = self.store.changes_since(g.since, limit + 1).map_err(err)?;
                let more = changes.len() > limit;
                changes.truncate(limit);
                if let (Some(c), Some(last)) = (self.conns.get_mut(&conn), changes.last()) {
                    c.synced_to = c.synced_to.max(last.seq);
                }
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

    /// After a write: moves cursors along and builds the push for everyone
    /// else. The writer, if it was caught up, is still caught up.
    fn wrote(
        &mut self,
        conn: ConnId,
        before: u64,
        applied: &[tab_domain::ops::Applied],
    ) -> Result<Option<ServerEnvelope>, StoreError> {
        let head = self.store.head()?;
        for (other, c) in self.conns.iter_mut() {
            if *other != conn || c.synced_to == before {
                c.synced_to = head;
            }
        }
        Ok((!applied.is_empty()).then(|| {
            ServerEnvelope::new(
                0,
                Res::Changes(pb::Changes {
                    changes: applied.iter().map(Into::into).collect(),
                    head,
                    more: false,
                }),
            )
        }))
    }

    // The error is the reply to send, so its size doesn't matter.
    #[allow(clippy::result_large_err)]
    fn import(&mut self, conn: ConnId, id: u64, im: pb::Import) -> Result<Handled, ServerEnvelope> {
        let err = |e| store_error(id, e);
        let conflict = |m: String| ServerEnvelope::error(id, pb::error::Code::Conflict, m);
        let changes = im
            .changes
            .into_iter()
            .map(tab_domain::ops::Applied::try_from)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| ServerEnvelope::error(id, pb::error::Code::BadRequest, e.to_string()))?;
        let before = self.store.head().map_err(err)?;
        let importing = self.conns.get(&conn).is_some_and(|c| c.importing);
        if before != 0 && !importing {
            return Err(conflict(format!(
                "this hub already has {before} changes; import only into an empty hub"
            )));
        }
        let head = migrate::import(&mut self.store, &changes).map_err(|e| match e {
            migrate::MigrateError::Store(e) => store_error(id, e),
            other => conflict(other.to_string()),
        })?;
        if let Some(c) = self.conns.get_mut(&conn) {
            c.importing = true;
        }
        let push = self.wrote(conn, before, &changes).map_err(err)?;
        Ok(Handled {
            reply: ServerEnvelope::new(id, Res::Imported(pb::Imported { head })),
            push,
            direct: vec![],
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
