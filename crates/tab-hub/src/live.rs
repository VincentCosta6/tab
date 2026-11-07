//! The live channel: watching and steering agent sessions from any device.
//!
//! A session runs on one device, the one named in the `device` field of its
//! record ([`session_record`]). Its stored transcript syncs through the
//! change log like everything else. What the log shouldn't hold (text as it
//! streams, tool progress, approval prompts) the runner publishes here, and
//! the hub passes it to the connections watching that session without
//! storing it. The other way, any device can steer a session: send it a
//! message, answer an approval, stop it. The hub routes that to the
//! runner's connections.
//!
//! Event and command bodies are the app's own JSON; the hub only routes
//! them. When a connection starts watching a session, the runner is told
//! ([`Watched`](pb::Watched)) so it can publish a snapshot for the late
//! joiner (the reply so far, open approvals).
//!
//! Handing a session to another device ("continue here") will move the
//! record's `device`, with a lease (`session:<id>`) to fence the old runner.

use tab_protocol::ServerEnvelope;
use tab_protocol::pb::{self, server_envelope::Body as Res};

use crate::{ConnId, Handled, Hub, HubStore};

/// The record kind and id of agent session `id`: the desktop app syncs its
/// `sessions` table row by row, keyed by the primary key as a JSON array.
pub fn session_record(id: &str) -> (&'static str, String) {
    ("sessions", serde_json::json!([id]).to_string())
}

/// Why a live request was refused.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Refused {
    code: pb::error::Code,
    message: String,
}

impl Refused {
    fn new(code: pb::error::Code, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub(crate) fn reply(self, id: u64) -> ServerEnvelope {
        ServerEnvelope::error(id, self.code, self.message)
    }
}

fn delivered(id: u64, n: usize) -> ServerEnvelope {
    ServerEnvelope::new(
        id,
        Res::Delivered(pb::Delivered {
            connections: n as u32,
        }),
    )
}

impl<S: HubStore> Hub<S> {
    /// The device running `session`, from its record.
    pub fn runner(&self, session: &str) -> Option<String> {
        let (kind, id) = session_record(session);
        let record = self.store.get(kind, &id).ok().flatten()?;
        record.body.get("device")?.as_str().map(str::to_string)
    }

    /// Connections of `device` that said hello.
    fn conns_of(&self, device: &str) -> Vec<ConnId> {
        self.conns
            .iter()
            .filter(|(_, c)| c.device.as_deref() == Some(device))
            .map(|(id, _)| *id)
            .collect()
    }

    /// Connections watching `session`.
    pub fn watchers(&self, session: &str) -> Vec<ConnId> {
        self.conns
            .iter()
            .filter(|(_, c)| c.watching.contains(session))
            .map(|(id, _)| *id)
            .collect()
    }

    /// Tells `session`'s runner how many connections watch it now.
    pub(crate) fn watched_notice(&self, session: &str) -> Vec<(ConnId, ServerEnvelope)> {
        let Some(runner) = self.runner(session) else {
            return vec![];
        };
        let watchers = self.watchers(session).len() as u32;
        self.conns_of(&runner)
            .into_iter()
            .map(|c| {
                (
                    c,
                    ServerEnvelope::new(
                        0,
                        Res::Watched(pb::Watched {
                            session: session.to_string(),
                            watchers,
                        }),
                    ),
                )
            })
            .collect()
    }

    pub(crate) fn watch(&mut self, conn: ConnId, id: u64, sessions: Vec<String>) -> Handled {
        let now: std::collections::BTreeSet<String> =
            sessions.into_iter().filter(|s| !s.is_empty()).collect();
        let before = match self.conns.get_mut(&conn) {
            Some(c) => std::mem::replace(&mut c.watching, now.clone()),
            None => Default::default(),
        };
        let direct: Vec<_> = before
            .symmetric_difference(&now)
            .flat_map(|s| self.watched_notice(s))
            .collect();
        Handled {
            reply: delivered(id, direct.len()),
            push: None,
            direct,
        }
    }

    pub(crate) fn publish(
        &mut self,
        conn: ConnId,
        device: &str,
        id: u64,
        p: pb::Publish,
    ) -> Result<Handled, Refused> {
        match self.runner(&p.session) {
            None => {
                return Err(Refused::new(
                    pb::error::Code::BadRequest,
                    format!("no session {} here", p.session),
                ));
            }
            Some(r) if r != device => {
                return Err(Refused::new(
                    pb::error::Code::Conflict,
                    format!("session {} runs on {r}, not {device}", p.session),
                ));
            }
            Some(_) => {}
        }
        let live = ServerEnvelope::new(
            0,
            Res::Live(pb::Live {
                session: p.session.clone(),
                from: device.to_string(),
                event_json: p.event_json,
            }),
        );
        let direct: Vec<_> = self
            .watchers(&p.session)
            .into_iter()
            .filter(|c| *c != conn)
            .map(|c| (c, live.clone()))
            .collect();
        Ok(Handled {
            reply: delivered(id, direct.len()),
            push: None,
            direct,
        })
    }

    pub(crate) fn steer(
        &mut self,
        device: &str,
        id: u64,
        st: pb::Steer,
    ) -> Result<Handled, Refused> {
        let Some(runner) = self.runner(&st.session) else {
            return Err(Refused::new(
                pb::error::Code::BadRequest,
                format!("no session {} here", st.session),
            ));
        };
        let targets = self.conns_of(&runner);
        if targets.is_empty() {
            return Err(Refused::new(
                pb::error::Code::Conflict,
                format!("{runner}, which runs this session, isn't connected"),
            ));
        }
        let steered = ServerEnvelope::new(
            0,
            Res::Steered(pb::Steered {
                session: st.session,
                from: device.to_string(),
                body_json: st.body_json,
            }),
        );
        Ok(Handled {
            reply: delivered(id, targets.len()),
            push: None,
            direct: targets.into_iter().map(|c| (c, steered.clone())).collect(),
        })
    }
}
