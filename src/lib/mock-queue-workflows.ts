// Browser stand-in for workflows on queue items: a run in each state on the
// first few items, so the queue's workflow pills and an item's runs list
// have something to show.

import { NOT_HANDLED } from "./mock-workflows";
import type { Chat, EngineEvent, ItemWorkflowState, ItemWorkflowStatus, QueueItem, RunStatus, RunView, Session, WorkflowRun } from "./types";

const ago = (min: number) => new Date(Date.now() - min * 60_000).toISOString();

let views: RunView[] | null = null;

function seed(items: QueueItem[]): RunView[] {
  const open = items.filter((i) => i.state === "open" && !i.parent_id);
  const run = (n: number, item: QueueItem | undefined, name: string, status: RunStatus, startedMin: number, tookSec: number | null, extra: Partial<WorkflowRun> = {}): WorkflowRun => ({
    id: `qrun-${n}`, workflow_id: `wf-q${n}`, workflow_name: name, version: 1, status, dry_run: false,
    trigger: item ? `item ingested: ${item.title}` : "by hand", trigger_key: null, item_id: item?.id ?? null, chain: [], steps: [],
    error: null, started_at: ago(startedMin), finished_at: tookSec == null ? null : new Date(Date.parse(ago(startedMin)) + tookSec * 1000).toISOString(), ...extra,
  });
  const view = (r: WorkflowRun, state: ItemWorkflowState | null, extra: Partial<RunView> = {}): RunView => ({ run: r, state, agents: [], started_by: null, caused_by: null, approval: null, ...extra });
  const [a, b, c, d, e, f, g] = open;
  const out: RunView[] = [];
  if (a) {
    const merge = { repo: "acme/shop", number: 482, method: "squash" };
    out.push(view(run(1, a, "Merge when green", "blocked", 4, 3, {
      error: "Merge a pull request is destructive. Allow destructive steps on this workflow to let it run.",
      steps: [
        { id: "checks", label: "Checks passed", status: "ok", input: null, output: {}, error: null, ms: 210 },
        { id: "merge", label: "Merge a pull request", status: "blocked", input: merge, output: null, error: "Merge a pull request is destructive. Allow destructive steps on this workflow to let it run.", ms: 0 },
      ],
    }), "waiting_for_you", { approval: { step_id: "merge", label: "Merge a pull request", reason: "It's destructive, and this workflow doesn't allow destructive steps on its own.", input: merge } }));
    out.push(view(run(11, a, "Small PRs get a review task", "succeeded", 50, 2), null));
  }
  if (b) out.push(view(run(2, b, "Triage CI failures", "running", 0.4, null), "running"));
  if (c) out.push(view(run(3, c, "Fix flaky tests", "waiting", 12, null), "waiting_on_agent", { agents: [{ step_id: "fix", job: "qjob-3", session: "qjob-chat-3", status: "working", needs_you: false }] }));
  if (d) out.push(view(run(4, d, "Draft a reply", "waiting", 25, null), "waiting_for_you", { agents: [{ step_id: "draft", job: "qjob-4", session: "qjob-chat-4", status: "needs_input", needs_you: true }] }));
  if (e) out.push(view(run(5, e, "Label new issues", "failed", 140, 4, { error: "GitHub said 502 while adding labels." }), "failed"));
  if (f) out.push(view(run(6, f, "Comment on review requests", "throttled", 30, 0, { error: "Held back to protect the rate budget." }), "held_back"));
  if (g) {
    const parent = run(70, c, "Fix flaky tests", "succeeded", 95, 1800);
    out.push(view(run(7, g, "Open a follow-up task", "succeeded", 18, 6, { started_by: "qjob-chat-70", caused_by: parent.id }), "done", {
      started_by: { session: "qjob-chat-70", title: "Workflow job: Fix the flaky checkout test", item_id: null, job: "qjob-70" },
      caused_by: { id: parent.id, workflow_name: parent.workflow_name, item_id: parent.item_id, exact: true },
      agents: [],
    }));
    out.push(view(parent, null, { agents: [{ step_id: "fix", job: "qjob-70", session: "qjob-chat-70", status: "idle", needs_you: false }] }));
  }
  return out;
}

const rank: ItemWorkflowState[] = ["waiting_for_you", "running", "waiting_on_agent", "failed", "held_back", "done"];

function statuses(vs: RunView[]): ItemWorkflowStatus[] {
  const by = new Map<string, RunView[]>();
  for (const v of vs) if (v.state && v.run.item_id) by.set(v.run.item_id, [...(by.get(v.run.item_id) ?? []), v]);
  return [...by.entries()].map(([item, xs]) => {
    const top = [...xs].sort((p, q) => rank.indexOf(p.state!) - rank.indexOf(q.state!))[0];
    const asking = top.agents.find((a) => a.needs_you);
    return { item_id: item, state: top.state!, run_id: top.run.id, workflow_id: top.run.workflow_id, workflow_name: top.run.workflow_name, at: top.run.finished_at ?? top.run.started_at, session: asking?.session ?? null, more: xs.length - 1 };
  });
}

function jobChat(job: string): Chat {
  const now = new Date().toISOString();
  const session: Session = { id: `${job}-chat`, item_id: null, agent: "claude_code", agent_session_id: null, title: "Workflow job", status: job === "qjob-4" ? "needs_input" : "idle", cwd: "~/code/shop", created_at: ago(25), updated_at: now };
  return {
    session, approvals: [], running: false,
    messages: [
      { id: 1, session_id: session.id, role: "user", kind: "text", content: "Draft a reply to the customer about the refund delay.", meta: null, created_at: ago(25) },
      { id: 2, session_id: session.id, role: "assistant", kind: "text", content: job === "qjob-4" ? "I have a draft ready. Should it promise the refund by Friday, or only say it's being looked at?" : "Pinned the clock in the checkout test and pushed `fix-flaky-checkout`.", meta: null, created_at: ago(20) },
    ],
  };
}

/** The seeded runs, for the Workflows tab's run lists. */
export function queueRuns(items: QueueItem[]): WorkflowRun[] {
  views ??= seed(items);
  return views.map((v) => v.run);
}

export function queueWorkflowMock(cmd: string, args: Record<string, any>, env: { items: () => QueueItem[]; emit: (e: EngineEvent) => void }): unknown {
  if (!["workflow_activity", "item_workflow_runs", "workflow_run_view", "workflow_run_answer", "job_chat_open", "job_chat_send"].includes(cmd)) return NOT_HANDLED;
  views ??= seed(env.items());
  switch (cmd) {
    case "workflow_activity": return statuses(views);
    case "item_workflow_runs": return views.filter((v) => v.run.item_id === args.itemId).sort((p, q) => q.run.started_at.localeCompare(p.run.started_at));
    case "workflow_run_view": return views.find((v) => v.run.id === args.runId) ?? null;
    case "workflow_run_answer": {
      const v = views.find((x) => x.run.id === args.runId);
      if (!v?.approval) throw "This run isn't waiting for an OK any more.";
      const now = new Date().toISOString();
      const steps = v.run.steps.map((s) => (s.id === v.approval!.step_id ? { ...s, status: args.approve ? "ok" as const : "blocked" as const, output: args.approve ? { merged: true } : null, error: args.approve ? null : "You declined this step." } : s));
      const r: WorkflowRun = { ...v.run, steps, status: args.approve ? "succeeded" : "stopped", error: args.approve ? null : `You declined ${v.approval.label}, so the run ended there.`, finished_at: now, approved: args.approve ? [{ step: v.approval.step_id, input: v.approval.input, at: now }] : [] };
      views = views.map((x) => (x === v ? { ...v, run: r, approval: null, state: args.approve ? "done" : null } : x));
      env.emit({ type: "workflow_run", run: r });
      return r;
    }
    case "job_chat_open": return jobChat(args.jobId);
    case "job_chat_send": return jobChat(args.jobId).session;
  }
  return NOT_HANDLED;
}
