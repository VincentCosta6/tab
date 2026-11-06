// The in-app browser: a few tabs shown in a dock beside the app. In the
// desktop shell each tab is a native webview (see browser_* in lib.rs); the
// store here is only the list of tabs, which one is showing, and whether the
// dock is expanded over the rest of the app.

import { useSyncExternalStore } from "react";
import { inTauri } from "./api";

export interface BrowserTab {
  id: string;
  url: string;
  title: string;
  loading: boolean;
}

interface State {
  tabs: BrowserTab[];
  active: string | null;
  expanded: boolean;
}

let state: State = { tabs: [], active: null, expanded: false };
const subs = new Set<() => void>();
const set = (next: Partial<State>) => {
  state = { ...state, ...next };
  subs.forEach((f) => f());
};

export const browserStore = {
  subscribe: (f: () => void) => (subs.add(f), () => void subs.delete(f)),
  get: () => state,
};
export const useBrowser = () => useSyncExternalStore(browserStore.subscribe, browserStore.get);

const withHost = (url: string) => (/^[a-z][a-z0-9+.-]*:/i.test(url) ? url : `https://${url}`);

/** Opens a link in a new browser tab (or focuses the tab already showing it). */
export function openInTabBrowser(url: string) {
  url = withHost(url.trim());
  if (!/^https?:\/\//i.test(url)) return;
  const existing = state.tabs.find((t) => t.url === url);
  if (existing) return set({ active: existing.id });
  const id = Math.random().toString(36).slice(2, 10);
  set({ tabs: [...state.tabs, { id, url, title: hostOf(url), loading: true }], active: id });
}

export function newBrowserTab() {
  openInTabBrowser("https://duckduckgo.com");
}

export function closeBrowserTab(id: string) {
  const i = state.tabs.findIndex((t) => t.id === id);
  const tabs = state.tabs.filter((t) => t.id !== id);
  const active = state.active === id ? (tabs[Math.min(i, tabs.length - 1)]?.id ?? null) : state.active;
  set({ tabs, active, expanded: tabs.length ? state.expanded : false });
  if (inTauri) void import("./api").then(({ api }) => api.browserClose(id)).catch(() => {});
}

export function closeBrowser() {
  [...state.tabs].forEach((t) => closeBrowserTab(t.id));
}

export const selectBrowserTab = (id: string) => set({ active: id });
export const setBrowserExpanded = (expanded: boolean) => set({ expanded });

export function patchBrowserTab(id: string, patch: Partial<BrowserTab>) {
  if (!state.tabs.some((t) => t.id === id)) return;
  set({ tabs: state.tabs.map((t) => (t.id === id ? { ...t, ...patch } : t)) });
}

export function hostOf(url: string) {
  try {
    return new URL(url).host.replace(/^www\./, "");
  } catch {
    return url;
  }
}

export { withHost };
