// Links open in tab first. A link a module has its own view for (a GitHub
// PR) opens that view; otherwise a link to something in the queue opens that
// item; anything else goes to the browser. Every in-app view keeps its own "Open on GitHub"
// style button, and ⌘, Ctrl or Shift click always goes to the browser.

import { api, openUrl } from "./api";

/**
 * What a connector says about a link it recognises (a GitHub PR, a Slack
 * thread, a Jira ticket, a Confluence or Google doc). The connector decides
 * every part, tab only draws it: a chip or pill with the title and `status`,
 * and a hover card with the author, age and `facts`.
 */
export interface LinkPreview {
  title: string;
  /** The one badge that matters most, in the connector's own words. */
  status?: import("./types").Status | null;
  author?: string | null;
  updated_at?: string | null;
  /** Labelled rows for the hover card: ["Checks", "Passing"]. */
  facts?: Array<[string, string]>;
  /** True while the answer is still being worked out; tab asks again shortly. */
  settling?: boolean;
  /** True while something is in progress (CI running): tab checks back every
   * 15s instead of every minute, without the 5s pace of `settling`. */
  active?: boolean;
  /** True once nothing about it can change (merged, closed): no need to keep checking. */
  final?: boolean;
}

/** What a module makes of a URL it recognises. */
export interface LinkKind {
  /** Item-style kind, for the brand mark (e.g. "github.pull_request"). */
  kind: string;
  source: string;
  /** "Pull request", "Issue", … */
  label: string;
  icon: string;
  /** A readable name when the link has no title of its own: "owner/repo #12". */
  title: string;
  /** The address without query or fragment, used to find a queue item. */
  canonical: string;
  /** Fetches the preview. Leave it out and the link shows as a plain pill. Cache on the backend; tab shares one result per link. */
  preview?: () => Promise<LinkPreview>;
  /** Opens the module's own view. Returns false when it can't right now. */
  open?: () => boolean;
}

export type LinkMatcher = (url: URL) => LinkKind | null;

const matchers: LinkMatcher[] = [];

/** Modules call this (from the registry) to claim the URLs they understand. */
export function registerLinks(...m: LinkMatcher[]) {
  matchers.push(...m);
}

export function parseLink(url: string | null | undefined): LinkKind | null {
  if (!url) return null;
  let u: URL;
  try {
    u = new URL(url);
  } catch {
    return null;
  }
  for (const m of matchers) {
    const k = m(u);
    if (k) return k;
  }
  return null;
}

/** Item the Work tab should show when it next mounts or hears `tab:open-item`. */
export let pendingOpen: string | null = null;
export function takePendingOpen() {
  const id = pendingOpen;
  pendingOpen = null;
  return id;
}

/** Switches to the Work tab with one item (and its chat) open. */
export function openItem(id: string) {
  pendingOpen = id;
  window.dispatchEvent(new CustomEvent("tab:open-item", { detail: id }));
}

type Modifiers = { metaKey?: boolean; ctrlKey?: boolean; shiftKey?: boolean };

/** Opens a link in tab when tab has a view for it, else in the browser. */
export async function openLink(url: string, e?: Modifiers) {
  if (e && (e.metaKey || e.ctrlKey || e.shiftKey)) return openUrl(url);
  if (/^https?:\/\//i.test(url)) {
    const link = parseLink(url);
    if (link?.open?.()) return;
    const item = await api.itemByUrl(link?.canonical ?? url).catch(() => null);
    if (item) return openItem(item.id);
  }
  return openUrl(url);
}
