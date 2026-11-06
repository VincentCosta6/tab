// Pure helpers for the review workspace: PR status, local edits (sample PRs
// and optimistic updates), the file tree, and small formatting bits.

import type { PrFile, PullReview, ReviewAction, ReviewThread, Tone } from "../../../lib/types";

export interface Pill { key: string; label: string; tone: Tone; icon: string; title?: string }

export function openThreads(pr: PullReview) {
  return pr.threads.filter((t) => !t.resolved && t.comments.some((c) => !c.pending)).length;
}

export function pendingComments(pr: PullReview) {
  return pr.threads.reduce((n, t) => n + t.comments.filter((c) => c.pending).length, 0);
}

export type RunState = "failing" | "cancelled" | "action" | "pending" | "success" | "neutral" | "skipped";

/** How GitHub groups a check run: its icon, colour and place in the list follow from this. */
export function runState(r: { status: string; conclusion?: string | null }): RunState {
  if (r.status !== "completed") return "pending";
  switch (r.conclusion) {
    case "success": return "success";
    case "skipped": return "skipped";
    case "neutral": return "neutral";
    case "cancelled": return "cancelled";
    case "action_required": return "action";
    case "stale": return "skipped";
    default: return "failing";
  }
}

/** GitHub's icon and colour for each state; skipped, neutral and cancelled are grey, never a green tick. */
export const runLook: Record<RunState, { icon: string; tone: string; rank: number }> = {
  failing: { icon: "x-circle", tone: "text-danger", rank: 0 },
  action: { icon: "alert", tone: "text-warning", rank: 1 },
  cancelled: { icon: "octagon", tone: "subtle", rank: 2 },
  pending: { icon: "circle-dot", tone: "text-warning", rank: 3 },
  success: { icon: "check-circle", tone: "text-success", rank: 4 },
  neutral: { icon: "square-dash", tone: "subtle", rank: 5 },
  skipped: { icon: "circle-slash", tone: "subtle", rank: 6 },
};

/** Runs in GitHub's order: failing first, then running, passed, and skipped last. */
export function sortedRuns<R extends { status: string; conclusion?: string | null; name: string }>(runs: R[]): R[] {
  return [...runs].sort((a, b) => runLook[runState(a)].rank - runLook[runState(b)].rank || a.name.localeCompare(b.name));
}

export function checkCounts(pr: PullReview) {
  const runs = pr.checks.runs;
  const n = (...s: RunState[]) => runs.filter((r) => s.includes(runState(r))).length;
  const failed = n("failing", "cancelled", "action");
  const pending = n("pending");
  const passed = n("success");
  const skipped = n("skipped", "neutral");
  return { failed, pending, passed, skipped, total: runs.length };
}

/** "1 failing, 12 successful, 6 skipped", like GitHub's merge box. */
export function checkSummary(c: ReturnType<typeof checkCounts>) {
  return [
    c.failed && `${c.failed} failing`,
    c.pending && `${c.pending} in progress`,
    c.passed && `${c.passed} successful`,
    c.skipped && `${c.skipped} skipped`,
  ].filter(Boolean).join(", ") || "No checks";
}

/** The PR's lifecycle state, shown first in the header. */
export function statePill(pr: PullReview): Pill {
  if (pr.state === "merged") return { key: "state", label: "Merged", tone: "accent", icon: "git-merge" };
  if (pr.state === "closed") return { key: "state", label: "Closed", tone: "danger", icon: "git-pull-request-closed" };
  if (pr.draft) return { key: "state", label: "Draft", tone: "neutral", icon: "git-pull-request-draft" };
  return { key: "state", label: "Open", tone: "success", icon: "git-pull-request" };
}

/** What stands between this PR and merging, most important first. */
export function statusPills(pr: PullReview): Pill[] {
  if (pr.state !== "open") return [];
  const out: Pill[] = [];
  const c = checkCounts(pr);
  const conflicts = pr.mergeable === "conflicting" || pr.merge_state === "dirty";
  const threads = openThreads(pr);
  const ciFailing = c.failed > 0 || pr.checks.state === "failure" || pr.checks.state === "error";
  const ciRunning = !ciFailing && (c.pending > 0 || pr.checks.state === "pending" || pr.checks.state === "expected");

  if (ciFailing) out.push({ key: "ci", label: "CI failing", tone: "danger", icon: "x-circle", title: `${c.failed} of ${c.total} checks failed` });
  else if (ciRunning) out.push({ key: "ci", label: "CI running", tone: "warning", icon: "clock", title: `${c.pending} of ${c.total} checks still running` });
  else if (c.total) out.push({ key: "ci", label: "CI passing", tone: "success", icon: "check", title: `${c.passed} of ${c.total} checks passed` });
  if (conflicts) out.push({ key: "conflicts", label: "Merge conflicts", tone: "danger", icon: "git-merge", title: `Conflicts with ${pr.base_ref}` });
  if (threads) out.push({ key: "threads", label: `${threads} open thread${threads === 1 ? "" : "s"}`, tone: "warning", icon: "message" });
  if (pr.review_decision === "changes_requested") out.push({ key: "review", label: "Changes requested", tone: "danger", icon: "file-diff" });
  else if (pr.review_decision === "approved") out.push({ key: "review", label: "Approved", tone: "success", icon: "check-circle" });
  if (pr.merge_state === "behind") out.push({ key: "behind", label: `Behind ${pr.base_ref}`, tone: "warning", icon: "arrow-down" });

  if (!pr.draft) {
    const mergeable = !conflicts && !ciFailing && !ciRunning && (pr.merge_state === "clean" || pr.merge_state === "has_hooks" || (pr.review_decision === "approved" && pr.merge_state !== "blocked"));
    if (mergeable) out.unshift({ key: "ready", label: "Ready to merge", tone: "success", icon: "git-merge" });
    else if (pr.review_decision !== "approved" && pr.review_decision !== "changes_requested") out.unshift({ key: "ready", label: "Ready for review", tone: "info", icon: "eye" });
  }
  return out;
}

/** GitHub's button label for the repo's merge method. */
export const mergeLabel: Record<string, string> = { squash: "Squash and merge", merge: "Merge pull request", rebase: "Rebase and merge" };

/**
 * Everything standing between this PR and a merge, like GitHub's merge box:
 * conflicts, branch rules (reviews, required checks), a stale branch.
 * `warnings` don't block (e.g. failing checks that aren't required).
 */
export function mergeReasons(pr: PullReview): { blockers: string[]; warnings: string[] } {
  const blockers: string[] = [];
  const warnings: string[] = [];
  const names = (rs: { name: string }[]) => rs.map((r) => r.name).join(", ");
  const required = pr.checks.runs.filter((r) => r.required);
  const failingReq = required.filter((r) => ["failing", "cancelled", "action"].includes(runState(r)));
  const pendingReq = required.filter((r) => runState(r) === "pending");
  const failingOpt = pr.checks.runs.filter((r) => !r.required && runState(r) === "failing");

  if (pr.mergeable === "conflicting" || pr.merge_state === "dirty") blockers.push("Merge conflicts must be resolved");
  if (pr.review_decision === "changes_requested") blockers.push("Changes requested");
  else if (pr.review_decision === "review_required") blockers.push("Review required: no approving review yet");
  if (failingReq.length) blockers.push(`${failingReq.length} required check${failingReq.length === 1 ? "" : "s"} failing: ${names(failingReq)}`);
  if (pendingReq.length) blockers.push(`${pendingReq.length} required check${pendingReq.length === 1 ? "" : "s"} still running: ${names(pendingReq)}`);
  if (pr.merge_state === "behind") blockers.push(`Branch is out of date with ${pr.base_ref}`);
  if (pr.merge_state === "unknown" || pr.mergeable === "unknown") blockers.push("GitHub is still checking mergeability");
  // GitHub says "blocked" but we found no reason above (e.g. a rule we can't see).
  if (pr.merge_state === "blocked" && !blockers.length) blockers.push("Blocked by branch rules (required checks or reviews)");
  if (failingOpt.length) warnings.push(`${failingOpt.length} check${failingOpt.length === 1 ? "" : "s"} failing, but not required: ${names(failingOpt)}`);
  return { blockers, warnings };
}

/** What the header's main button does for this viewer. `null` means the usual "Submit review". */
export type PrimaryAction =
  | { kind: "ready"; label: string }
  | { kind: "reopen"; label: string }
  | { kind: "merge"; label: string; blockers: string[]; warnings: string[] };

export function primaryAction(pr: PullReview): PrimaryAction | null {
  if (pr.state === "merged") return null;
  // Reviewers still review; the contextual button is for the PR's author.
  if (!pr.viewer || pr.author !== pr.viewer) return null;
  if (pr.state === "closed") return { kind: "reopen", label: "Reopen pull request" };
  if (pr.draft) return { kind: "ready", label: "Open for review" };
  const { blockers, warnings } = mergeReasons(pr);
  return { kind: "merge", label: mergeLabel[pr.merge_method] ?? "Merge pull request", blockers, warnings };
}

let tempId = 0;
const temp = (p: string) => `local-${p}-${Date.now()}-${tempId++}`;

/** Applies an action to a PR locally: all of sample mode, and optimistic updates for real PRs. */
export function applyLocal(pr: PullReview, a: ReviewAction): PullReview {
  const now = new Date().toISOString();
  const mine = (body: string, pending: boolean) => ({
    id: temp("comment"), database_id: null, author: pr.viewer || "you", avatar: null, body, created_at: now, pending, url: null, viewer_did_author: true,
  });
  const threads = (f: (t: ReviewThread) => ReviewThread | null) => pr.threads.map(f).filter((t): t is ReviewThread => !!t);
  switch (a.type) {
    case "mark_viewed":
      return { ...pr, files: pr.files.map((f) => (f.filename === a.path ? { ...f, viewed: a.viewed ? "viewed" : "unviewed" } : f)) };
    case "mark_viewed_many": {
      const set = new Set(a.paths);
      return { ...pr, files: pr.files.map((f) => (set.has(f.filename) ? { ...f, viewed: a.viewed ? "viewed" : "unviewed" } : f)) };
    }
    case "add_thread":
      return {
        ...pr,
        pending_review: a.pending ? (pr.pending_review ?? { id: pr.sample ? temp("review") : "", body: "" }) : pr.pending_review,
        threads: [...pr.threads, {
          id: temp("thread"), path: a.path, line: a.line, start_line: a.start_line, original_line: a.line, side: a.side,
          resolved: false, outdated: false, can_resolve: true, can_reply: true, diff_hunk: null, comments: [mine(a.body, a.pending)],
        }],
      };
    case "reply":
      return {
        ...pr,
        pending_review: a.pending ? (pr.pending_review ?? { id: pr.sample ? temp("review") : "", body: "" }) : pr.pending_review,
        threads: threads((t) => (t.id === a.thread_id ? { ...t, comments: [...t.comments, mine(a.body, a.pending)] } : t)),
      };
    case "edit_comment":
      return { ...pr, threads: threads((t) => ({ ...t, comments: t.comments.map((c) => (c.id === a.comment_id ? { ...c, body: a.body } : c)) })) };
    case "delete_comment":
      return {
        ...pr,
        threads: threads((t) => {
          const comments = t.comments.filter((c) => c.id !== a.comment_id);
          return comments.length ? { ...t, comments } : null;
        }),
      };
    case "resolve":
      return { ...pr, threads: threads((t) => (t.id === a.thread_id ? { ...t, resolved: a.resolved } : t)) };
    case "submit":
      return {
        ...pr,
        pending_review: null,
        review_decision: a.event === "approve" ? "approved" : a.event === "request_changes" ? "changes_requested" : pr.review_decision,
        threads: threads((t) => ({ ...t, comments: t.comments.map((c) => ({ ...c, pending: false })) })),
      };
    case "discard":
      return {
        ...pr,
        pending_review: null,
        threads: threads((t) => {
          const comments = t.comments.filter((c) => !c.pending);
          return comments.length ? { ...t, comments } : null;
        }),
      };
    case "set_state":
      return { ...pr, state: a.state };
    case "set_draft":
      return { ...pr, draft: a.draft, merge_state: a.draft ? "draft" : pr.merge_state === "draft" ? "blocked" : pr.merge_state };
    case "merge":
      return { ...pr, state: "merged" };
    case "update_branch":
      return { ...pr, merge_state: pr.merge_state === "behind" ? "blocked" : pr.merge_state };
  }
}

export interface TreeNode { name: string; path: string; dir: boolean; children: TreeNode[] }

export function buildTree(paths: string[]): TreeNode[] {
  const root: TreeNode = { name: "", path: "", dir: true, children: [] };
  const dirs = new Map<string, TreeNode>([["", root]]);
  for (const p of paths) {
    const parts = p.split("/");
    let parent = root;
    for (let i = 0; i < parts.length; i++) {
      const path = parts.slice(0, i + 1).join("/");
      const last = i === parts.length - 1;
      if (last) parent.children.push({ name: parts[i], path, dir: false, children: [] });
      else {
        let d = dirs.get(path);
        if (!d) {
          d = { name: parts[i], path, dir: true, children: [] };
          dirs.set(path, d);
          parent.children.push(d);
        }
        parent = d;
      }
    }
  }
  const sort = (n: TreeNode) => {
    n.children.sort((a, b) => (a.dir === b.dir ? a.name.localeCompare(b.name) : a.dir ? -1 : 1));
    n.children.forEach(sort);
  };
  sort(root);
  return root.children;
}

export const statusLetter: Record<string, string> = { added: "A", removed: "D", modified: "M", renamed: "R", copied: "C", changed: "M", unchanged: "" };
export const statusTone: Record<string, string> = { added: "success", removed: "danger", modified: "warning", renamed: "info", copied: "info", changed: "warning" };

export function splitPath(p: string) {
  const i = p.lastIndexOf("/");
  return { dir: i >= 0 ? p.slice(0, i) : "", name: i >= 0 ? p.slice(i + 1) : p };
}

export function timeAgo(iso: string) {
  const s = Math.max(0, (Date.now() - new Date(iso).getTime()) / 1000);
  if (s < 60) return "just now";
  if (s < 3600) return `${Math.floor(s / 60)}m ago`;
  if (s < 86400) return `${Math.floor(s / 3600)}h ago`;
  if (s < 86400 * 30) return `${Math.floor(s / 86400)}d ago`;
  return new Date(iso).toLocaleDateString();
}

export function isViewed(f: PrFile) {
  return f.viewed === "viewed";
}

/** Lines a thread covers, on its side. */
export function threadRange(t: ReviewThread): [number, number] | null {
  if (!t.line) return null;
  return [t.start_line && t.start_line < t.line ? t.start_line : t.line, t.line];
}

/** A tree of changed files with single-child folders merged ("src/lib"), like VS Code. */
export function compactTree(nodes: TreeNode[]): TreeNode[] {
  return nodes.map((n) => {
    let cur = n;
    while (cur.dir && cur.children.length === 1 && cur.children[0].dir) {
      const c = cur.children[0];
      cur = { ...c, name: `${cur.name}/${c.name}` };
    }
    return cur.dir ? { ...cur, children: compactTree(cur.children) } : cur;
  });
}

/** Paths in the order the tree shows them, folders first. */
export function treeOrder(nodes: TreeNode[]): string[] {
  return nodes.flatMap((n) => (n.dir ? treeOrder(n.children) : [n.path]));
}

export function filesUnder(n: TreeNode): string[] {
  return n.dir ? n.children.flatMap(filesUnder) : [n.path];
}
