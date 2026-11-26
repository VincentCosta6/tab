// The phone's calls into its Rust core (apps/mobile/src-tauri). Types for
// chats and runners are the desktop's (src/lib/types.ts), which mirror the
// same Rust structs.

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { EngineEvent, Runner, Session, SessionMessage } from "../../../src/lib/types";

export type { EngineEvent, Session, SessionMessage };

export interface AccountView { user_id: string; email: string | null; name: string | null; provider: string }
export interface AccountState { configured: boolean; account: AccountView | null; connected: boolean; error: string | null }

/** Where the person asked a chat to run (mirrors tab_domain::delegate::RunOn). */
export type RunOn = { type: "auto" } | { type: "cloud" } | { type: "computers" } | { type: "runner"; id: string };
/** tab cloud's runner, as tab cloud reports it. */
export type Cloud = "not_on_plan" | "asleep" | "awake";
/** Who will most likely take a chat (mirrors tab_domain::delegate::Expect). */
export type Expect =
  | { type: "runner"; id: string; name: string; kind: "computer" | "cloud" }
  | { type: "wake_cloud" }
  | { type: "waits"; why: string }
  | { type: "never"; why: string };
export interface Plan { placement: { type: string; minutes?: number }; runner?: string; expect: Expect }

export type RunnerView = Omit<Runner, "this_app">;
export interface Pending { job: string; title: string; item: string; created_at: string; given_up: string | null }
export interface Overview { sessions: Session[]; pending: Pending[]; runners: RunnerView[]; cloud: Cloud; device: string }
export interface NewChat { prompt: string; title?: string; item?: string; repo?: string; harness?: string; on: RunOn }

export const api = {
  account: () => invoke<AccountState>("account"),
  signIn: (provider: "apple" | "google") => invoke<void>("sign_in", { provider }),
  signOut: () => invoke<void>("sign_out"),
  overview: () => invoke<Overview>("overview"),
  session: (session: string) => invoke<Session | null>("session", { session }),
  messages: (session: string) => invoke<SessionMessage[]>("messages", { session }),
  planChat: (chat: NewChat) => invoke<Plan>("plan_chat", { chat }),
  startChat: (chat: NewChat) => invoke<{ job: string; plan: Plan }>("start_chat", { chat }),
  jobSession: (job: string) => invoke<string | null>("job_session", { job }),
  steer: (session: string, text: string) => invoke<void>("steer", { session, text }),
  stop: (session: string) => invoke<void>("stop", { session }),
};

export const onEvent = (f: (e: EngineEvent) => void): Promise<UnlistenFn> => listen<EngineEvent>("tab://event", (e) => f(e.payload));
/** Something changed that the UI should read again (synced, or too many events to replay). */
export const onChanged = (f: () => void): Promise<UnlistenFn> => listen("tab://changed", () => f());
export const onAccount = (f: () => void): Promise<UnlistenFn> => listen("tab://account", () => f());

/** What the picker and the composer say about where a chat will run. */
export function expectLabel(e: Expect): string {
  switch (e.type) {
    case "runner": return e.kind === "cloud" ? "Runs on tab cloud" : `Runs on ${e.name}`;
    case "wake_cloud": return "Runs on tab cloud (waking it up)";
    case "waits": return e.why;
    case "never": return e.why;
  }
}
