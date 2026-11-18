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
  /** Opened by an agent (docs/agent-browser.md). Display only: the
   * desktop shell builds its webview from tab-core's registry, not from
   * this flag. */
  agent?: AgentTabState;
}

/** What the dock shows about an agent's tab. */
export interface AgentTabState {
  sessionId: string;
  /** The approved plan's goal and progress, and how many actions it used. */
  goal?: string | null;
  steps?: number | null;
  done?: number;
  actions?: number;
  maxActions?: number;
  /** The site whose login the person lent to this tab. */
  lent?: string | null;
  /** Why the person has the tab (they took over, or a handover). */
  paused?: string | null;
  /** What the agent waits to do until the person watches. */
  waiting?: string | null;
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

/** An agent opened, moved or closed one of its tabs, or its state changed. */
export function onAgentBrowser(e: { session_id: string; tab_id: string; action: string; url: string; detail?: unknown }) {
  const tab = state.tabs.find((t) => t.id === e.tab_id);
  const d = (e.detail ?? {}) as Record<string, unknown>;
  const agent = (patch: Partial<AgentTabState>) =>
    tab?.agent && patchBrowserTab(tab.id, { agent: { ...tab.agent, ...patch } });
  if (e.action === "open" && !tab) {
    const t: BrowserTab = { id: e.tab_id, url: e.url, title: hostOf(e.url), loading: true, agent: { sessionId: e.session_id } };
    set({ tabs: [...state.tabs, t], active: e.tab_id });
  } else if (e.action === "navigate" && tab) {
    patchBrowserTab(tab.id, { url: e.url, loading: true });
    if (inTauri) void import("./api").then(({ api }) => api.browserNav(tab.id, "go", e.url)).catch(() => {});
  } else if (e.action === "close" && tab) {
    closeBrowserTab(tab.id);
  } else if (e.action === "status") {
    agent({
      goal: d.goal as string | null, steps: d.steps as number | null, done: d.done as number,
      actions: d.actions as number, maxActions: d.max_actions as number,
      lent: d.lent as string | null, paused: d.paused as string | null,
    });
  } else if (e.action === "handover" && tab) {
    agent({ paused: (d.why as string) ?? "the agent handed it to you" });
    set({ active: tab.id });
  } else if (e.action === "waiting" && tab) {
    // Watch mode: bring the tab forward so the person sees what's waiting.
    agent({ waiting: (d.what as string) ?? null });
    if (d.what) set({ active: tab.id });
  }
}

/** The person hands an agent's tab back. */
export function handBack(id: string) {
  const tab = state.tabs.find((t) => t.id === id);
  if (tab?.agent) patchBrowserTab(id, { agent: { ...tab.agent, paused: null } });
  if (inTauri) void import("./api").then(({ api }) => api.browserAgentResume(id)).catch(() => {});
}

/** Stop: ends the agent's browsing in that chat (plan, lent logins, tabs). */
export function stopAgent(sessionId: string) {
  if (inTauri) void import("./api").then(({ api }) => api.browserAgentStop(sessionId)).catch(() => {});
  set({ tabs: state.tabs.filter((t) => t.agent?.sessionId !== sessionId) });
  if (!state.tabs.some((t) => t.id === state.active)) set({ active: state.tabs[0]?.id ?? null, expanded: state.tabs.length ? state.expanded : false });
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
