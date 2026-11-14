// tab's own updates: the hourly check, and installing what it found. The
// Settings badge and Settings → Updates share this, so an install started in one shows
// its progress in the other.

import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useSyncExternalStore } from "react";
import { api, onAppUpdateProgress } from "../../lib/api";

export const HOUR = 60 * 60_000;

export function useAppVersion() {
  return useQuery({ queryKey: ["app-version"], queryFn: api.appVersion, retry: false });
}

/** Checks on start and then every hour while the app runs. */
export function useAppUpdate() {
  const version = useAppVersion();
  const check = useQuery({
    queryKey: ["app-update"],
    queryFn: api.appUpdateCheck,
    enabled: !!version.data?.updates,
    staleTime: HOUR,
    refetchInterval: HOUR,
    refetchIntervalInBackground: true,
    retry: false,
  });
  return { version, check };
}

export type InstallState =
  | { phase: "idle" }
  | { phase: "downloading"; downloaded: number; total: number | null }
  | { phase: "restarting" }
  | { phase: "failed"; error: string };

let state: InstallState = { phase: "idle" };
const listeners = new Set<() => void>();
const set = (next: InstallState) => {
  state = next;
  listeners.forEach((l) => l());
};

export function useInstall() {
  const qc = useQueryClient();
  const current = useSyncExternalStore(
    (l) => (listeners.add(l), () => void listeners.delete(l)),
    () => state,
  );
  const install = async () => {
    if (state.phase === "downloading" || state.phase === "restarting") return;
    set({ phase: "downloading", downloaded: 0, total: null });
    const off = await onAppUpdateProgress((p) => {
      set(p.total != null && p.downloaded >= p.total ? { phase: "restarting" } : { phase: "downloading", ...p });
    });
    try {
      // On success the app restarts (or, on Windows, the installer takes
      // over), so this normally doesn't return in a desktop build.
      await api.appUpdateInstall();
      set({ phase: "restarting" });
      // Only the browser mock gets here: pretend the restart happened.
      setTimeout(() => {
        set({ phase: "idle" });
        void qc.refetchQueries({ queryKey: ["app-version"] });
        void qc.refetchQueries({ queryKey: ["app-update"] });
      }, 1200);
    } catch (e) {
      set({ phase: "failed", error: String((e as { message?: string })?.message ?? e) });
    } finally {
      off();
    }
  };
  return { state: current, install };
}

/** 0 to 100, or null while the size isn't known. */
export function percent(s: InstallState) {
  return s.phase === "downloading" && s.total ? Math.min(100, Math.round((s.downloaded / s.total) * 100)) : null;
}
