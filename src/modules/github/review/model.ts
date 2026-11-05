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

export function checkCounts(pr: PullReview) {
  const runs = pr.checks.runs;
  const failed = runs.filter((r) => r.status === "completed" && ["failure", "timed_out", "cancelled", "action_required", "startup_failure", "error"].includes(r.conclusion ?? "")).length;
  const pending = runs.filter((r) => r.status !== "completed").length;
  const passed = runs.filter((r) => r.status === "completed" && ["success", "neutral", "skipped"].includes(r.conclusion ?? "")).length;
  return { failed, pending, passed, total: runs.length };
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
