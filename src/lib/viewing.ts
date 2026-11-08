// What the person is looking at right now, for the app header: the chat
// session on screen, if any. A view sets it while it's mounted.

import { useSyncExternalStore } from "react";
import type { Session } from "./types";

let current: Session | null = null;
const listeners = new Set<() => void>();

export function setViewedSession(s: Session | null) {
  if (s === current || (s && current && s.id === current.id && s.status === current.status && s.device === current.device)) return;
  current = s;
  for (const l of listeners) l();
}

export function useViewedSession(): Session | null {
  return useSyncExternalStore(
    (l) => (listeners.add(l), () => void listeners.delete(l)),
    () => current,
  );
}
