// Workflows on an item: a pill for its one workflow status (next to the
// agent's pill on the queue row and in the header), and a compact list of
// its runs, each linking to the run, its agent's chat and what started it.
// "Waiting for you" always leads somewhere: the step to approve, or the
// agent's question in its chat.

import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Button, cx } from "../../design/components";
import { Icon } from "../../design/icons";
import { api } from "../../lib/api";
import { timeAgo } from "../../lib/format";
import { openItem } from "../../lib/links";
import { keys } from "../../lib/query";
import type { Chat, ItemWorkflowState, ItemWorkflowStatus, RunStatus, RunView, Tone } from "../../lib/types";
import { openRun } from "../workflows/runLinks";
import { Composer, Transcript } from "./Chat";
import "./workflowStatus.css";

/** Label and tone per state, in SessionPill's language; `pulse` animates like the agent states. */
export const workflowLook: Record<ItemWorkflowState, { label: string; tone: Tone; pulse?: "working" | "needs_input" | "monitoring" }> = {
  waiting_for_you: { label: "Waiting for you", tone: "warning", pulse: "needs_input" },
  running: { label: "Running", tone: "info", pulse: "working" },
  waiting_on_agent: { label: "Waiting on an agent", tone: "accent", pulse: "monitoring" },
  failed: { label: "Failed", tone: "danger" },
  held_back: { label: "Held back", tone: "neutral" },
  done: { label: "Done", tone: "success" },
};

/** What a run's own status reads as, when it no longer shows on the item. */
const runWord: Record<RunStatus, { label: string; tone: Tone }> = {
  running: { label: "Running", tone: "info" },
  waiting: { label: "Waiting on an agent", tone: "accent" },
  succeeded: { label: "Done", tone: "success" },
  stopped: { label: "Stopped", tone: "neutral" },
  failed: { label: "Failed", tone: "danger" },
  blocked: { label: "Blocked", tone: "warning" },
  throttled: { label: "Held back", tone: "neutral" },
  loop_stopped: { label: "Loop stopped", tone: "neutral" },
};

export function WorkflowPill({ status, state, title, className }: { status?: ItemWorkflowStatus; state?: ItemWorkflowState; title?: string; className?: string }) {
  const s = workflowLook[status?.state ?? state ?? "done"];
  const more = status?.more ? ` · +${status.more} more` : "";
  return (
    <span
      className={cx("session-pill", "wf-pill", s.pulse ? `is-${s.pulse}` : "is-still", `tone-${s.tone}`, className)}
      title={title ?? (status ? `Workflow "${status.workflow_name}": ${s.label.toLowerCase()} · ${timeAgo(status.at)}${more}` : undefined)}
    >
      <Icon name="workflow" size={11} />
      {s.label}
    </span>
  );
}

function duration(r: RunView["run"]): string | null {
  if (!r.finished_at) return null;
  const ms = Date.parse(r.finished_at) - Date.parse(r.started_at);
  if (ms < 1000) return "under 1s";
  if (ms < 60_000) return `${Math.round(ms / 1000)}s`;
  if (ms < 3600_000) return `${Math.round(ms / 60_000)}m`;
  return `${(ms / 3600_000).toFixed(1)}h`;
}

/** An item's workflow runs, newest first. Nothing when it has none. */
export function ItemWorkflows({ itemId }: { itemId: string }) {
  const { data: runs = [] } = useQuery({ queryKey: keys.itemWorkflowRuns(itemId), queryFn: () => api.itemWorkflowRuns(itemId), staleTime: 0 });
  const [all, setAll] = useState(false);
  const [chat, setChat] = useState<{ job: string; title: string } | null>(null);
  if (!runs.length) return null;
  // What needs you first, then the newest.
  const shown = all ? runs : runs.filter((r, i) => i < 3 || r.approval || r.agents.some((a) => a.needs_you));
  return (
    <section className="item-workflows" aria-label="Workflow runs">
      <div className="item-workflows-head">
        <Icon name="workflow" size={12} /> Workflows
        {runs.length > shown.length && <button className="link-button" onClick={() => setAll(true)}>Show all {runs.length}</button>}
      </div>
      {shown.map((v) => <RunLine key={v.run.id} view={v} onChat={(job, title) => setChat({ job, title })} />)}
      {chat && <JobChat job={chat.job} title={chat.title} onClose={() => setChat(null)} />}
    </section>
  );
}

/** One run: what it is, how it went, and where it leads. */
export function RunLine({ view: v, onChat, inPanel }: { view: RunView; onChat: (job: string, title: string) => void; inPanel?: boolean }) {
  const r = v.run;
  const look = v.state ? workflowLook[v.state] : runWord[r.status];
  const took = duration(r);
  const asking = v.agents.find((a) => a.needs_you && a.session);
  const chats = v.agents.filter((a) => a.session);
  return (
    <div className={cx("wf-runline", v.state === "waiting_for_you" && "asks")}>
      {!inPanel && <div className="wf-runline-main">
        {v.state ? <WorkflowPill state={v.state} title={r.error ?? undefined} /> : <span className={cx("session-pill", "wf-pill", "is-still", `tone-${look.tone}`)} title={r.error ?? undefined}><Icon name="workflow" size={11} />{look.label}</span>}
        <span className="wf-runline-name" title={r.trigger}>{r.workflow_name}</span>
        <span className="wf-runline-when subtle" title={new Date(r.started_at).toLocaleString()}>{timeAgo(r.started_at)}{took && ` · took ${took}`}</span>
      </div>}
      <div className="wf-runline-links">
        {!inPanel && <button className="link-button" onClick={() => openRun(r.id)}>Open run</button>}
        {chats.map((a) => (
          <button key={a.job} className="link-button" onClick={() => onChat(a.job, `${r.workflow_name} · agent`)}>
            Agent chat{chats.length > 1 ? ` (${a.step_id})` : ""}
          </button>
        ))}
        {v.caused_by && (
          <button className="link-button" title={v.caused_by.exact ? undefined : "The run of that workflow just before this one"} onClick={() => openRun(v.caused_by!.id)}>
            Caused by {v.caused_by.workflow_name}
          </button>
        )}
        {v.started_by && (
          <button className="link-button" onClick={() => (v.started_by!.job ? onChat(v.started_by!.job, v.started_by!.title) : v.started_by!.item_id ? openItem(v.started_by!.item_id) : undefined)}>
            Started by {v.started_by.title || "an agent chat"}
          </button>
        )}
        {!v.started_by && r.started_by && <span className="subtle">Started by an agent</span>}
      </div>
      {v.approval && <Approve view={v} />}
      {asking && (
        <div className="wf-runline-ask">
          <Icon name="message" size={13} />
          <span>The agent on this run asked you something.</span>
          <Button size="sm" variant="primary" onClick={() => onChat(asking.job, `${r.workflow_name} · agent`)}>Answer in chat</Button>
        </div>
      )}
    </div>
  );
}

/** Approve one blocked step for this one run, or decline and end the run. */
function Approve({ view: v }: { view: RunView }) {
  const qc = useQueryClient();
  const [busy, setBusy] = useState<"yes" | "no" | null>(null);
  const [error, setError] = useState<string | null>(null);
  const a = v.approval!;
  const answer = async (approve: boolean) => {
    if (approve && !confirm(`Run "${a.label || a.step_id}" on this run? Only this run; the workflow still asks next time.`)) return;
    setBusy(approve ? "yes" : "no");
    setError(null);
    try {
      await api.workflowRunAnswer(v.run.id, approve);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
      qc.invalidateQueries({ queryKey: ["item-workflow-runs"] });
      qc.invalidateQueries({ queryKey: keys.workflowActivity });
      qc.invalidateQueries({ queryKey: ["workflow-run-view", v.run.id] });
      qc.invalidateQueries({ queryKey: ["workflow-runs"] });
    }
  };
  return (
    <div className="wf-approve">
      <div className="wf-approve-text">
        <strong>{a.label || a.step_id} needs your OK</strong>
        {a.reason && <span className="subtle">{a.reason}</span>}
        {a.input != null && <details><summary>What it would do</summary><pre>{JSON.stringify(a.input, null, 2)}</pre></details>}
        {error && <span className="wf-approve-error">{error}</span>}
      </div>
      <div className="wf-approve-actions">
        <Button size="sm" onClick={() => answer(false)} disabled={!!busy}>{busy === "no" ? "Declining…" : "Decline"}</Button>
        <Button size="sm" variant="primary" icon="check" onClick={() => answer(true)} disabled={!!busy}>{busy === "yes" ? "Running…" : "Approve and carry on"}</Button>
      </div>
    </div>
  );
}

/** A workflow job's agent chat, over the page. */
export function JobChat({ job, title, onClose }: { job: string; title: string; onClose: () => void }) {
  const qc = useQueryClient();
  const key = ["chat", "job", job];
  const { data: chat } = useQuery<Chat>({ queryKey: key, queryFn: () => api.jobChatOpen(job), staleTime: 0, refetchOnMount: "always" });
  const send = async (text: string) => {
    qc.setQueryData<Chat>(key, (c) => (c ? { ...c, running: true, messages: [...c.messages, { id: -Date.now(), session_id: c.session?.id ?? "", role: "user", kind: "text", content: text, meta: null, created_at: new Date().toISOString() }] } : c));
    try {
      await api.jobChatSend(job, text);
    } finally {
      qc.invalidateQueries({ queryKey: key });
    }
  };
  return (
    <div className="score-overlay" onMouseDown={onClose}>
      <div className="job-chat" role="dialog" aria-label={title} onMouseDown={(e) => e.stopPropagation()}>
        <header className="job-chat-head">
          <strong>{title}</strong>
          <button className="row-action" title="Close" onClick={onClose}><Icon name="x" size={14} /></button>
        </header>
        <div className="job-chat-transcript">
          <Transcript chat={chat} onSend={send} intro={chat && !chat.session && <p className="subtle wf-pad">This agent's chat isn't on this device. It ran on another of your computers, or in tab cloud.</p>} />
        </div>
        {chat?.session && <Composer chat={chat} onSend={(t) => send(t)} placeholder="Answer the agent…" />}
      </div>
    </div>
  );
}
