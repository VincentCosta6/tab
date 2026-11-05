// Data for the review workspace: the PR, the local checkout, file contents,
// search, and every action, with optimistic updates. Sample PRs never touch
// the backend: their checkout is simulated and edits stay in the cache.

import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api, onWorkspaceProgress } from "../../../lib/api";
import { keys } from "../../../lib/query";
import type { PrRef, PullReview, ReviewAction, ReviewWorkspace, SearchHit, SearchQuery, WorkspaceProgress } from "../../../lib/types";
import { applyLocal } from "./model";

export function usePull(repo: string, number: number, sample: boolean) {
  return useQuery({
    queryKey: keys.pull(repo, number),
    queryFn: () => api.githubPull(repo, number, sample),
    // Show the cached PR at once, then refresh it on every open.
    refetchOnMount: "always",
  });
}

const prRef = (pr: PullReview): PrRef => ({
  repo: pr.repo, number: pr.number, node_id: pr.node_id, head_sha: pr.head_sha, pending_review_id: pr.pending_review?.id || null,
});

/** Runs review actions one at a time, applying each locally first. */
export function useActions(pr: PullReview | undefined) {
  const qc = useQueryClient();
  const queue = useRef<Promise<unknown>>(Promise.resolve());
  return useCallback(
    (action: ReviewAction): Promise<void> => {
      if (!pr) return Promise.resolve();
      const key = keys.pull(pr.repo, pr.number);
      const run = async () => {
        const prev = qc.getQueryData<PullReview>(key);
        if (prev) qc.setQueryData(key, applyLocal(prev, action));
        if (pr.sample) return;
        // Read the latest PR so a pending review created a moment ago is used.
        const current = qc.getQueryData<PullReview>(key) ?? pr;
        try {
          await api.githubReviewAction(prRef(prev ?? current), action);
        } catch (e) {
          if (prev) qc.setQueryData(key, prev);
          throw e;
        }
        // Viewed marks don't change anything else; skip the refetch.
        if (action.type !== "mark_viewed" && action.type !== "mark_viewed_many") {
          // Context chips and cards show this PR's status too.
          void qc.invalidateQueries({ queryKey: ["link-summary", `https://github.com/${pr.repo}/pull/${pr.number}`] });
          await qc.invalidateQueries({ queryKey: key });
        }
      };
      const next = queue.current.then(run, run);
      queue.current = next.catch(() => {});
      return next;
    },
    [pr, qc],
  );
}

export type StepId = WorkspaceProgress["step"];
export const STEPS: StepId[] = ["clone", "fetch", "checkout", "index"];
export interface StepState { state: "waiting" | "running" | "done" | "error"; message: string; percent: number | null }
export interface Boot {
  phase: "booting" | "ready" | "error" | "remote";
  steps: Record<StepId, StepState>;
  ws: ReviewWorkspace | null;
  error: string | null;
  retry: () => void;
  /** Review without a local checkout, reading files through the API. */
  goRemote: () => void;
}

const initialSteps = (): Record<StepId, StepState> => ({
  clone: { state: "waiting", message: "", percent: null },
  fetch: { state: "waiting", message: "", percent: null },
  checkout: { state: "waiting", message: "", percent: null },
  index: { state: "waiting", message: "", percent: null },
});

/** Workspaces already prepared this session, so reopening a PR is instant. */
const ready = new Map<string, ReviewWorkspace>();

export function useWorkspace(pr: PullReview | undefined): Boot {
  const [steps, setSteps] = useState(initialSteps);
  const [ws, setWs] = useState<ReviewWorkspace | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [remote, setRemote] = useState(false);
  const [attempt, setAttempt] = useState(0);
  const id = pr ? `${pr.repo}#${pr.number}@${pr.head_sha}` : "";

  useEffect(() => {
    if (!pr || remote) return;
    const cached = ready.get(id);
    if (cached) {
      setWs(cached);
      setSteps(Object.fromEntries(STEPS.map((s) => [s, { state: "done", message: "", percent: 100 }])) as Record<StepId, StepState>);
      return;
    }
    let live = true;
    setSteps(initialSteps());
    setError(null);
    setWs(null);
    const set = (step: StepId, s: Partial<StepState>) => live && setSteps((all) => ({ ...all, [step]: { ...all[step], ...s } }));

    if (pr.sample) {
      // A short, honest-looking boot for sample data: nothing is cloned.
      const files = [...new Set([...pr.sample_tree.map((f) => f.path), ...pr.files.filter((f) => f.status !== "removed").map((f) => f.filename)])].sort();
      const timers: number[] = [];
      const at = (ms: number, f: () => void) => timers.push(window.setTimeout(() => live && f(), ms));
      at(0, () => set("clone", { state: "running", message: `Cloning ${pr.repo}`, percent: 0 }));
      for (let i = 1; i <= 8; i++) at(i * 70, () => set("clone", { percent: Math.round((i / 8) * 100), message: `Receiving objects: ${Math.round((i / 8) * 100)}%` }));
      at(620, () => { set("clone", { state: "done", message: `Cloned ${pr.repo}`, percent: 100 }); set("fetch", { state: "running", message: `Fetching #${pr.number}` }); });
      at(880, () => { set("fetch", { state: "done", message: `Fetched #${pr.number}` }); set("checkout", { state: "running", message: `Checking out ${pr.head_ref}` }); });
      at(1120, () => { set("checkout", { state: "done", message: `Checked out ${pr.head_ref}` }); set("index", { state: "running", message: "Indexing files" }); });
      at(1320, () => {
        set("index", { state: "done", message: `Indexed ${files.length} files` });
        const w = { path: `~/tab/workspaces/${pr.repo}/pr-${pr.number}`, head_sha: pr.head_sha, merge_base: pr.base_sha, files, reused: false };
        ready.set(id, w);
        setWs(w);
      });
      return () => {
        live = false;
        timers.forEach(clearTimeout);
      };
    }

    let off: (() => void) | undefined;
    onWorkspaceProgress((p) => {
      if (p.repo !== pr.repo || p.number !== pr.number) return;
      set(p.step, { state: p.state, message: p.message, percent: p.percent });
    }).then((u) => (live ? (off = u) : u()));
    api
      .workspacePrepare({ repo: pr.repo, number: pr.number, head_sha: pr.head_sha, base_sha: pr.base_sha, base_ref: pr.base_ref })
      .then((w) => {
        if (!live) return;
        ready.set(id, w);
        setWs(w);
      })
      .catch((e) => live && setError(String(e)));
    return () => {
      live = false;
      off?.();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [id, remote, attempt]);

  return {
    phase: remote ? "remote" : ws ? "ready" : error ? "error" : "booting",
    steps,
    ws,
    error,
    retry: () => { setRemote(false); setAttempt((n) => n + 1); },
    goRemote: () => setRemote(true),
  };
}

/** Where file contents come from: the sample, the local clone, or the API. */
export interface Source {
  pr: PullReview;
  ws: ReviewWorkspace | null;
  baseSha: string;
}

export function useSource(pr: PullReview | undefined, ws: ReviewWorkspace | null): Source | null {
  return useMemo(() => (pr ? { pr, ws, baseSha: ws?.merge_base ?? pr.base_sha } : null), [pr, ws]);
}

function sampleText(pr: PullReview, side: "base" | "head", path: string) {
  const f = pr.files.find((f) => f.filename === path || f.previous_filename === path);
  if (f) return (side === "base" ? f.original : f.modified) ?? "";
  return pr.sample_tree.find((t) => t.path === path)?.content ?? "";
}

export function readFile(src: Source, side: "base" | "head", path: string): Promise<string> {
  const { pr, ws } = src;
  if (pr.sample) return Promise.resolve(sampleText(pr, side, path));
  const sha = side === "base" ? src.baseSha : pr.head_sha;
  return ws ? api.workspaceFile(pr.repo, sha, path) : api.githubFile(pr.repo, sha, path);
}

export function useFileText(src: Source | null, side: "base" | "head", path: string | null, enabled = true) {
  const sha = src ? (side === "base" ? src.baseSha : src.pr.head_sha) : "";
  return useQuery({
    queryKey: [...keys.file(src?.pr.repo ?? "", sha, path ?? ""), src?.ws ? "local" : "api"],
    queryFn: () => readFile(src!, side, path!),
    enabled: !!src && !!path && enabled,
  });
}

export function useSearch(src: Source | null, q: SearchQuery) {
  const sample = src?.pr.sample;
  return useQuery<SearchHit[]>({
    queryKey: ["review-search", src?.pr.repo, src?.pr.number, src?.pr.head_sha, q],
    enabled: !!src && !!q.query && (!!src.ws || !!sample),
    staleTime: 30_000,
    gcTime: 60_000,
    queryFn: async () => {
      const { pr } = src!;
      if (!sample) return api.workspaceSearch(pr.repo, pr.number, q);
      return searchText(
        src!.ws!.files.map((p) => [p, sampleText(pr, "head", p)]),
        q,
      );
    },
  });
}

/** Client-side search for sample data, with the same options as git grep. */
export function searchText(files: [string, string][], q: SearchQuery): SearchHit[] {
  let re: RegExp;
  try {
    const src = q.regex ? q.query : q.query.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    re = new RegExp(q.whole_word ? `\\b(?:${src})\\b` : src, q.case_sensitive ? "" : "i");
  } catch {
    return [];
  }
  const hits: SearchHit[] = [];
  for (const [path, text] of files) {
    text.split("\n").forEach((line, i) => {
      const m = re.exec(line);
      if (m) hits.push({ path, line: i + 1, column: m.index + 1, text: line });
    });
  }
  return hits;
}
