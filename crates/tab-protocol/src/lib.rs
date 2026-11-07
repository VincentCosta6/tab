//! `tab-protocol` is the shared API layer: the messages a client (the
//! desktop UI, the phone, a runner, the CLI) and a hub exchange.
//!
//! The messages are Protobuf, defined in `proto/tab/hub/v1/hub.proto`, which
//! is the source of truth. This crate generates the Rust types with prost
//! ([`pb`]), and the TypeScript types for the React UI come from the same
//! file through protobuf-es (`buf.gen.yaml` at the repo root). Frames are
//! binary: a [`pb::ClientEnvelope`] or [`pb::ServerEnvelope`] per WebSocket
//! message to a cloud hub, or passed in-process when the hub runs inside
//! the desktop app.
//!
//! **No gRPC.** Browsers and the desktop webview can only reach gRPC through
//! a gRPC-Web proxy, Cloudflare Workers can't host a gRPC server (tonic
//! needs tokio and raw HTTP/2), and the hub's main channel is a long-lived
//! WebSocket on a Durable Object, which gRPC doesn't use. We keep what
//! Protobuf is good at (a compact binary format, generated types in Rust and
//! TypeScript, a clear compatibility story) and skip the RPC framework.
//!
//! **Compatibility:** fields and messages are only ever added; removed ones
//! are `reserved`; `buf breaking` checks this in CI. A hub answers every
//! version from [`MIN_PROTOCOL_VERSION`] to [`PROTOCOL_VERSION`].
//!
//! [`convert`] maps between these wire types and `tab-domain`'s types.

pub mod convert;

/// Types generated from `hub.proto`.
#[allow(clippy::all, missing_docs)]
pub mod pb {
    include!(concat!(env!("OUT_DIR"), "/tab.hub.v1.rs"));
}

pub use pb::{ClientEnvelope, ServerEnvelope};
use pb::{client_envelope, server_envelope};
use prost::Message;

/// The protocol version this build speaks.
pub const PROTOCOL_VERSION: u32 = 1;
/// The oldest version a hub built from this crate still answers.
pub const MIN_PROTOCOL_VERSION: u32 = 1;
/// Changes a hub sends per page when the client doesn't say.
pub const DEFAULT_CHANGES_PER_PAGE: u32 = 200;
/// Most changes a hub sends in one page.
pub const MAX_CHANGES_PER_PAGE: u32 = 1000;
/// Search hits or neighbors returned when the client doesn't say.
pub const DEFAULT_RESULTS: u32 = 20;
/// Most search hits or neighbors returned at once.
pub const MAX_RESULTS: u32 = 200;

impl ClientEnvelope {
    pub fn new(request_id: u64, body: client_envelope::Body) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            request_id,
            body: Some(body),
        }
    }
}

impl ServerEnvelope {
    pub fn new(request_id: u64, body: server_envelope::Body) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            request_id,
            body: Some(body),
        }
    }

    /// An error reply to `request_id`.
    pub fn error(request_id: u64, code: pb::error::Code, message: impl Into<String>) -> Self {
        Self::new(
            request_id,
            server_envelope::Body::Error(pb::Error {
                code: code as i32,
                message: message.into(),
            }),
        )
    }
}

/// Encodes any message as a binary frame.
pub fn encode<M: Message>(msg: &M) -> Vec<u8> {
    msg.encode_to_vec()
}

/// Decodes one client frame, or gives the error reply to send back.
/// A frame with no body (a newer client's message this hub doesn't know)
/// is answered as unsupported rather than dropped.
pub fn decode_client(frame: &[u8]) -> Result<(u64, client_envelope::Body), ServerEnvelope> {
    let env = ClientEnvelope::decode(frame)
        .map_err(|e| ServerEnvelope::error(0, pb::error::Code::BadRequest, e.to_string()))?;
    let id = env.request_id;
    if !(MIN_PROTOCOL_VERSION..=PROTOCOL_VERSION).contains(&env.protocol_version) {
        return Err(ServerEnvelope::error(
            id,
            pb::error::Code::Unsupported,
            format!(
                "protocol v{} isn't supported; this hub speaks v{MIN_PROTOCOL_VERSION} to v{PROTOCOL_VERSION}",
                env.protocol_version
            ),
        ));
    }
    env.body.map(|b| (id, b)).ok_or_else(|| {
        ServerEnvelope::error(
            id,
            pb::error::Code::Unsupported,
            "this hub doesn't know that request",
        )
    })
}

/// Decodes one server frame.
pub fn decode_server(frame: &[u8]) -> Result<ServerEnvelope, prost::DecodeError> {
    ServerEnvelope::decode(frame)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip() {
        let env = ClientEnvelope::new(
            7,
            client_envelope::Body::GetChanges(pb::GetChanges { since: 3, limit: 0 }),
        );
        let bytes = encode(&env);
        let (id, body) = decode_client(&bytes).unwrap();
        assert_eq!(id, 7);
        assert_eq!(body, env.body.unwrap());
    }

    #[test]
    fn unknown_fields_and_requests_are_survivable() {
        // A newer client adds a field (number 99) to its envelope: older hubs
        // skip it.
        let mut bytes = encode(&ClientEnvelope::new(
            1,
            client_envelope::Body::Search(pb::Search {
                query: "login".into(),
                limit: 5,
            }),
        ));
        bytes.extend_from_slice(&[0x98, 0x06, 0x01]); // field 99, varint 1
        assert!(decode_client(&bytes).is_ok());

        // A request type this hub doesn't know decodes with no body.
        let mut unknown = encode(&ClientEnvelope {
            protocol_version: 1,
            request_id: 2,
            body: None,
        });
        unknown.extend_from_slice(&[0xfa, 0x06, 0x00]); // field 111, empty bytes
        let err = decode_client(&unknown).unwrap_err();
        assert_eq!(err.request_id, 2);
        assert!(matches!(
            err.body,
            Some(server_envelope::Body::Error(pb::Error { code, .. })) if code == pb::error::Code::Unsupported as i32
        ));
    }

    #[test]
    fn bad_versions_and_garbage_get_errors() {
        let old = encode(&ClientEnvelope {
            protocol_version: 99,
            request_id: 4,
            body: Some(client_envelope::Body::Hello(pb::Hello::default())),
        });
        let err = decode_client(&old).unwrap_err();
        assert_eq!(err.request_id, 4);
        assert!(decode_client(&[0xff, 0xff, 0xff]).is_err());
    }
}
