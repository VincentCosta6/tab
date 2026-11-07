// The hub protocol, generated from crates/tab-protocol/proto by protobuf-es
// (`pnpm proto`). Nothing in the UI talks to a hub yet; this is here so the
// TypeScript side shares the exact messages the Rust hub speaks.
import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import {
  ClientEnvelopeSchema,
  ServerEnvelopeSchema,
  type ClientEnvelope,
  type ServerEnvelope,
} from "./gen/tab/hub/v1/hub_pb.js";

export * from "./gen/tab/hub/v1/hub_pb.js";

/** Must match `tab_protocol::PROTOCOL_VERSION`. */
export const PROTOCOL_VERSION = 1;

/** Encodes a request as a binary frame. */
export function encodeRequest(
  requestId: bigint,
  body: ClientEnvelope["body"],
): Uint8Array {
  return toBinary(
    ClientEnvelopeSchema,
    create(ClientEnvelopeSchema, {
      protocolVersion: PROTOCOL_VERSION,
      requestId,
      body,
    }),
  );
}

/** Decodes a frame from the hub. */
export function decodeReply(frame: Uint8Array): ServerEnvelope {
  return fromBinary(ServerEnvelopeSchema, frame);
}
