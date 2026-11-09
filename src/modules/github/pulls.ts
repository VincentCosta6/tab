import { api } from "../../lib/api";
import type { LinkMatcher } from "../../lib/links";
import { whereAmI, type Origin } from "../../lib/nav";
import { toPreview } from "./preview";

/** A PR opened from elsewhere, and where it was opened from (so Back returns there). */
export interface LinkedPull { repo: string; number: number; from?: Origin }

/** A PR the Reviews tab should open when it next mounts or hears `tab:open-pr`. */
let pendingPull: LinkedPull | null = null;
export function takePendingPull() {
  const p = pendingPull;
  pendingPull = null;
  return p;
}

/** Set by the shell while the Reviews tab is available. */
let showReviews: (() => void) | null = null;
export function setReviewsOpener(f: (() => void) | null) {
  showReviews = f;
}

/** Opens a PR in the review workspace, if the Reviews tab is available. */
export function openPull(repo: string, number: number): boolean {
  if (!showReviews) return false;
  pendingPull = { repo, number, from: whereAmI() };
  showReviews();
  window.dispatchEvent(new CustomEvent("tab:open-pr"));
  return true;
}

/** github.com PRs, issues and commits. PRs open in the review workspace. */
export const githubLinks: LinkMatcher = (u) => {
  if (u.hostname !== "github.com" && u.hostname !== "www.github.com") return null;
  const [owner, repo, what, id] = u.pathname.split("/").filter(Boolean);
  if (!owner || !repo) return null;
  const base = `https://github.com/${owner}/${repo}`;
  const name = `${owner}/${repo}`;
  const preview = (what: string, id: string) => () => api.githubLinkSummary(name, what, id).then(toPreview);
  if (what === "pull" && /^\d+$/.test(id ?? ""))
    return { kind: "github.pull_request", source: "github", label: "Pull request", icon: "git-pull-request", title: `${name} #${id}`, canonical: `${base}/pull/${id}`, preview: preview("pull", id), open: () => openPull(name, Number(id)) };
  if (what === "issues" && /^\d+$/.test(id ?? ""))
    return { kind: "github.issue", source: "github", label: "Issue", icon: "ticket", title: `${name} #${id}`, canonical: `${base}/issues/${id}`, preview: preview("issues", id) };
  if (what === "commit" && id)
    return { kind: "github.commit", source: "github", label: "Commit", icon: "git-branch", title: `${name} @ ${id.slice(0, 7)}`, canonical: `${base}/commit/${id}`, preview: preview("commit", id) };
  if (!what) return { kind: "github.repo", source: "github", label: "Repository", icon: "folder", title: name, canonical: base };
  return null;
};

/** `https://github.com/owner/repo/pull/123[/...]` → repo and number. */
export function parsePullUrl(url: string | null | undefined): { repo: string; number: number } | null {
  const m = url?.match(/github\.com\/([^/]+\/[^/]+)\/pull\/(\d+)/);
  return m ? { repo: m[1], number: Number(m[2]) } : null;
}
