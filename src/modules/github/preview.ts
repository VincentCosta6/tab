import type { LinkPreview } from "../../lib/links";
import type { LinkSummary, Tone } from "../../lib/types";

const checkStatus: Record<string, { label: string; tone: Tone }> = {
  failing: { label: "CI failing", tone: "danger" },
  pending: { label: "CI running", tone: "warning" },
  passing: { label: "CI passing", tone: "success" },
};

/**
 * One badge for a linked PR or issue: the thing that most decides what
 * happens next, like GitHub's merge box. Merged or closed, then conflicts,
 * failing CI, changes requested, unresolved threads, CI running, review
 * needed, and finally ready to merge.
 */
export function linkStatus(s: LinkSummary): { label: string; tone: Tone } | null {
  if (s.state === "merged") return { label: "Merged", tone: "accent" };
  if (s.state === "closed") return { label: "Closed", tone: "neutral" };
  if (s.merge_state === "dirty") return { label: "Conflicts", tone: "danger" };
  if (s.checks === "failing") return checkStatus.failing;
  if (s.review_decision === "changes_requested") return { label: "Changes requested", tone: "danger" };
  if (s.open_threads) return { label: `${s.open_threads} open thread${s.open_threads === 1 ? "" : "s"}`, tone: "warning" };
  if (s.state === "draft") return { label: "Draft", tone: "neutral" };
  if (s.checks === "pending") return checkStatus.pending;
  if (s.review_decision === "review_required") return { label: "Needs review", tone: "info" };
  if (s.merge_state === "behind") return { label: "Behind base", tone: "warning" };
  if (s.merge_state === "blocked") return { label: "Blocked", tone: "warning" };
  if (s.merge_state === "clean" || s.merge_state === "has_hooks") return { label: "Ready to merge", tone: "success" };
  if (s.merge_state === "unstable" || s.merge_state === "mergeable") return { label: "Mergeable", tone: "success" };
  if (s.state === "open" && (!s.merge_state || s.merge_state === "unknown") && s.review_decision !== undefined) return { label: "Checking…", tone: "neutral" };
  if (s.checks === "passing") return checkStatus.passing;
  return s.state === "open" ? { label: "Open", tone: "success" } : null;
}

const reviewLabel: Record<string, string> = { approved: "Approved", changes_requested: "Changes requested", review_required: "Review required" };
const checksLabel: Record<string, string> = { passing: "Passing", failing: "Failing", pending: "Running" };
const mergeLabel: Record<string, string> = {
  clean: "Ready to merge",
  has_hooks: "Ready to merge",
  mergeable: "No conflicts",
  unstable: "Mergeable, some checks not required",
  dirty: "Has conflicts",
  behind: "Behind the base branch",
  blocked: "Blocked by branch rules",
};

/** GitHub's answer, in the shape every connector's preview takes. */
export function toPreview(s: LinkSummary): LinkPreview {
  const facts: Array<[string, string]> = [];
  if (s.checks) facts.push(["Checks", checksLabel[s.checks] ?? s.checks]);
  if (s.review_decision) facts.push(["Review", reviewLabel[s.review_decision] ?? s.review_decision]);
  if (s.open_threads) facts.push(["Threads", `${s.open_threads} unresolved`]);
  if (s.state === "open" && s.merge_state && mergeLabel[s.merge_state]) facts.push(["Merge", mergeLabel[s.merge_state]]);
  return {
    title: s.title,
    status: linkStatus(s),
    author: s.author,
    updated_at: s.updated_at,
    facts,
    // "unknown" means GitHub is still working out mergeability.
    settling: s.state === "open" && s.review_decision !== undefined && (!s.merge_state || s.merge_state === "unknown"),
  };
}
