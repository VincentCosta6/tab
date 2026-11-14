// The tab cloud account API, generated from
// crates/tab-protocol/proto/tab/cloud/v1/account.proto by protobuf-es
// (`pnpm proto`). The edge Worker in tab-infra is built from the same file,
// so these are exactly the messages it sends. The app's Rust passes the
// bytes through unchanged (the cloud_* Tauri commands) and they're decoded
// here.
import { fromBinary, type DescMessage, type MessageShape } from "@bufbuild/protobuf";
import { timestampDate } from "@bufbuild/protobuf/wkt";
import { ErrorCode, type Plan } from "./gen/tab/cloud/v1/account_pb.js";

export * from "./gen/tab/cloud/v1/account_pb.js";

/** Why a cloud call failed. `code` is the cloud's ErrorCode, or UNREACHABLE / UNAUTHENTICATED set by the app. */
export interface CloudFailure {
  code: ErrorCode;
  message: string;
}

/** Decodes the bytes a cloud_* command returns. */
export function decode<D extends DescMessage>(schema: D, bytes: ArrayBuffer | Uint8Array | number[]): MessageShape<D> {
  return fromBinary(schema, bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes));
}

/** Whatever a cloud_* command rejected with, as a CloudFailure. */
export function asFailure(e: unknown): CloudFailure {
  return e && typeof e === "object" && "code" in e && "message" in e
    ? (e as CloudFailure)
    : { code: ErrorCode.INTERNAL, message: String(e) };
}

/** When a plan ends, or null if it renews. */
export const planEnds = (plan: Plan | undefined): Date | null => (plan?.endsAt ? timestampDate(plan.endsAt) : null);
