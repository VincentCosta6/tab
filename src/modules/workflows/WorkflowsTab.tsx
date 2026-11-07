// The workflow builder: a list of workflows, a canvas showing one as a flow
// (trigger, then steps), an inspector whose forms come from each action's
// descriptor, the runs log, and an assistant that builds workflows for you.
//
// Everything a step can do comes from connector metadata (`actions` on each
// connector), so a new connector action shows up here with no UI change.

import { useEffect, useMemo, useRef, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Badge, Button, cx, SourceIcon, Toggle } from "../../design/components";
import { Icon } from "../../design/icons";
import { api, onEngineEvent } from "../../lib/api";
import { timeAgo } from "../../lib/format";
import { useConnectors, useItems } from "../../lib/hooks";
import type {
  ActionField, ActionInfo, Change, Chat, Condition, ConnectorInfo, Effect, Issue, Op, Step, StepRun, StepStatus, Tone, Trigger, Workflow, WorkflowRun, RunStatus,
} from "../../lib/types";
import { AgentStatus, Composer, Transcript, openItem } from "../work/Chat";
import "./workflows.css";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

const changes: { value: Change; label: string }[] = [
  { value: "ingested", label: "New" },
  { value: "updated", label: "Updated" },
  { value: "ranked", label: "Ranked" },
  { value: "snoozed", label: "Snoozed" },
  { value: "woke", label: "Woke" },
  { value: "done", label: "Done" },
  { value: "reopened", label: "Reopened" },
];

const ops: { value: Op; label: string; noValue?: boolean }[] = [
  { value: "equals", label: "is" },
  { value: "not_equals", label: "is not" },
  { value: "contains", label: "contains" },
  { value: "not_contains", label: "doesn't contain" },
  { value: "starts_with", label: "starts with" },
  { value: "matches", label: "matches (*)" },
  { value: "in", label: "is one of" },
  { value: "gt", label: ">" },
  { value: "lt", label: "<" },
  { value: "exists", label: "is set", noValue: true },
  { value: "missing", label: "is empty", noValue: true },
];

const effectTone: Record<Effect, Tone> = { read: "neutral", write: "info", destructive: "danger" };
const effectLabel: Record<Effect, string> = { read: "Reads", write: "Writes", destructive: "Destructive" };

const runTone: Record<RunStatus, Tone> = {
  running: "info", succeeded: "success", stopped: "neutral", failed: "danger", blocked: "warning", throttled: "warning", loop_stopped: "warning",
};
const runLabel: Record<RunStatus, string> = {
  running: "Running", succeeded: "Ran", stopped: "Stopped", failed: "Failed", blocked: "Needs your OK", throttled: "Held back", loop_stopped: "Loop stopped",
};
const stepTone: Record<StepStatus, Tone> = { ok: "success", skipped: "neutral", stopped: "neutral", failed: "danger", blocked: "warning", throttled: "warning", dry: "accent" };
const stepLabel: Record<StepStatus, string> = { ok: "Done", skipped: "Skipped", stopped: "Stopped here", failed: "Failed", blocked: "Blocked", throttled: "Held back", dry: "Would run" };

function blank(): Workflow {
  return {
    id: "", name: "New workflow", description: "", enabled: false, allow_destructive: false,
    trigger: { type: "item", changes: ["ingested"], sources: [], kinds: [], when: [] },
    steps: [], limits: { max_runs_per_hour: 30, cooldown_minutes: 0 }, created_by: "user", version: 0, updated_at: null,
  };
}

const clone = <T,>(v: T): T => JSON.parse(JSON.stringify(v));

function findAction(connectors: ConnectorInfo[], connector: string, action: string): ActionInfo | undefined {
  return connectors.find((c) => c.id === connector)?.actions?.find((a) => a.id === action);
}

function uniqueId(w: Workflow, base: string) {
  const b = base.replace(/[^a-z0-9_]/gi, "_").toLowerCase().replace(/^_+|_+$/g, "") || "step";
  let id = b;
  for (let n = 2; w.steps.some((s) => s.id === id); n++) id = `${b}_${n}`;
  return id;
}

function describeTrigger(t: Trigger, connectors: ConnectorInfo[]): string {
  switch (t.type) {
    case "item": {
      const ch = t.changes.map((c) => changes.find((x) => x.value === c)?.label.toLowerCase() ?? c).join(" or ");
      const src = t.sources.map((s) => connectors.find((c) => c.id === s)?.name ?? s).join(", ");
      return `When an item is ${ch || "…"}${src ? ` from ${src}` : ""}`;
    }
    case "signal": return `When a workflow signals ${t.name || "…"}`;
    case "schedule": return t.every_minutes % 1440 === 0 ? `Every ${t.every_minutes / 1440 === 1 ? "day" : `${t.every_minutes / 1440} days`}` : t.every_minutes % 60 === 0 ? `Every ${t.every_minutes / 60} hour${t.every_minutes === 60 ? "" : "s"}` : `Every ${t.every_minutes} minutes`;
    case "manual": return "When you run it";
  }
}

function text(v: unknown): string {
  if (v == null) return "";
  if (typeof v === "string") return v;
  return JSON.stringify(v);
}

/** Variables a step can use: the item, earlier steps' outputs, the signal. */
function variables(w: Workflow, upTo: number, connectors: ConnectorInfo[]): { path: string; label: string }[] {
  const out: { path: string; label: string }[] = [];
  if (w.trigger.type === "item" || w.trigger.type === "manual") {
    out.push(
      { path: "item.title", label: "Title" }, { path: "item.body", label: "Body" }, { path: "item.url", label: "Link" },
      { path: "item.actor", label: "From" }, { path: "item.context", label: "Where" }, { path: "item.kind", label: "Kind" },
      { path: "item.score", label: "Score" }, { path: "item.id", label: "Item id" },
    );
    const src = w.trigger.type === "item" ? w.trigger.sources : [];
    if (!src.length || src.includes("github")) out.push({ path: "item.metadata.repo", label: "Repo" }, { path: "item.metadata.number", label: "PR / issue #" });
    if (!src.length || src.includes("email")) out.push({ path: "item.metadata.folder", label: "Mail folder" }, { path: "item.metadata.uid", label: "Mail UID" });
  }
  if (w.trigger.type === "signal") out.push({ path: "signal", label: "Signal payload" });
  for (const s of w.steps.slice(0, upTo)) {
    if (s.type === "action") {
      for (const f of findAction(connectors, s.connector, s.action)?.outputs ?? []) out.push({ path: `steps.${s.id}.${f.key}`, label: `${s.id}: ${f.label}` });
    } else if (s.type === "set") for (const k of Object.keys(s.values)) out.push({ path: `steps.${s.id}.${k}`, label: `${s.id}: ${k}` });
    else if (s.type === "command") {
      out.push({ path: `steps.${s.id}.exit_code`, label: `${s.id}: Exit code` }, { path: `steps.${s.id}.stdout`, label: `${s.id}: Output` }, { path: `steps.${s.id}.stderr`, label: `${s.id}: Errors` });
    }
  }
  out.push({ path: "now", label: "Now" });
  return out;
}

// ---------------------------------------------------------------------------
// The tab
// ---------------------------------------------------------------------------

type Panel = "edit" | "runs" | "assistant";

export function WorkflowsTab() {
  const qc = useQueryClient();
  const { data: list = [] } = useQuery({ queryKey: ["workflows"], queryFn: api.workflowsList, staleTime: 0 });
  const { data: templates = [] } = useQuery({ queryKey: ["workflow-templates"], queryFn: api.workflowTemplates });
  const { data: connectors = [] } = useConnectors();
  const [openId, setOpenId] = useState<string | null>(() => localStorage.getItem("tab.workflow") || null);
  const [draft, setDraft] = useState<Workflow | null>(null);
  const [dirty, setDirty] = useState(false);
  const [selected, setSelected] = useState<string>("trigger");
  const [panel, setPanel] = useState<Panel>("edit");
  const [dry, setDry] = useState<WorkflowRun | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [testItem, setTestItem] = useState<string>("");
  const [view, setView] = useState<"builder" | "runs">(() => (localStorage.getItem("tab.workflows.view") === "runs" ? "runs" : "builder"));
  const { data: allRuns = [] } = useQuery({ queryKey: ["workflow-runs", "all"], queryFn: () => api.workflowRuns(null, 200), staleTime: 0 });
  const lastRun = useMemo(() => {
    const m = new Map<string, WorkflowRun>();
    for (const r of allRuns) if (!r.dry_run && !m.has(r.workflow_id)) m.set(r.workflow_id, r);
    return m;
  }, [allRuns]);

  useEffect(() => {
    try { localStorage.setItem("tab.workflows.view", view); } catch { /* private window */ }
  }, [view]);

  // Open the remembered (or first) workflow once the list loads.
  useEffect(() => {
    if (draft || !list.length) return;
    const w = list.find((x) => x.id === openId) ?? list[0];
    setDraft(clone(w));
    setOpenId(w.id);
  }, [list, draft, openId]);

  useEffect(() => {
    try { if (openId) localStorage.setItem("tab.workflow", openId); } catch { /* private window */ }
  }, [openId]);

  // When the assistant saves the workflow that's open (or the new draft), show it.
  const live = useRef({ draft, dirty });
  live.current = { draft, dirty };
  useEffect(() => {
    let off: (() => void) | undefined;
    onEngineEvent(async (e) => {
      if (e.type !== "workflows_changed" || e.by !== "agent") return;
      const { draft: d, dirty: isDirty } = live.current;
      if (isDirty && d?.id === e.id) return;
      if (!d || d.id === e.id || (!d.id && !isDirty) || !d.steps.length) {
        const all = await qc.fetchQuery({ queryKey: ["workflows"], queryFn: api.workflowsList, staleTime: 0 });
        const w = all.find((x) => x.id === e.id);
        if (w) { setDraft(clone(w)); setOpenId(w.id); setDirty(false); setDry(null); }
      }
    }).then((u) => (off = u));
    return () => off?.();
  }, [qc]);

  const { data: issues = [] } = useQuery({
    queryKey: ["workflow-issues", JSON.stringify(draft)],
    queryFn: () => api.workflowValidate(draft!),
    enabled: !!draft,
    staleTime: 0,
    placeholderData: (p) => p,
  });

  const edit = (f: (w: Workflow) => void) => {
    setDraft((d) => {
      if (!d) return d;
      const n = clone(d);
      f(n);
      return n;
    });
    setDirty(true);
    setDry(null);
  };

  const open = (w: Workflow) => {
    if (dirty && !confirm("Discard your unsaved changes?")) return;
    setView("builder");
    setDraft(clone(w));
    setOpenId(w.id || null);
    setDirty(!w.id);
    setSelected("trigger");
    setDry(null);
    setError(null);
  };

  const act = async (label: string, f: () => Promise<void>) => {
    setBusy(label);
    setError(null);
    try { await f(); } catch (e) { setError(String(e)); } finally { setBusy(null); }
  };

  const save = () => act("save", async () => {
    const { workflow } = await api.workflowSave(draft!);
    setDraft(clone(workflow));
    setOpenId(workflow.id);
    setDirty(false);
    await qc.invalidateQueries({ queryKey: ["workflows"] });
  });

  const setEnabled = (on: boolean, allowDestructive?: boolean, allowCommands?: boolean) => act("enable", async () => {
    if (dirty) {
      const { workflow, issues: found } = await api.workflowSave(draft!);
      setDirty(false);
      setOpenId(workflow.id);
      if (found.some((i) => i.error)) { setDraft(clone(workflow)); throw new Error("Fix the problems below before turning it on."); }
      const w = await api.workflowSetEnabled(workflow.id, on, allowDestructive, allowCommands);
      setDraft(clone(w));
    } else {
      const w = await api.workflowSetEnabled(draft!.id, on, allowDestructive, allowCommands);
      setDraft(clone(w));
    }
    await qc.invalidateQueries({ queryKey: ["workflows"] });
  });

  const runDry = () => act("dry", async () => {
    const run = await api.workflowRun(draft!, testItem || null, true);
    setDry(run);
  });

  const runNow = () => act("run", async () => {
    if (!confirm("Run it for real now? Its write steps will change things.")) return;
    await api.workflowRun(draft!, testItem || null, false);
    setPanel("runs");
    await qc.invalidateQueries({ queryKey: ["workflow-runs"] });
  });

  const remove = () => act("delete", async () => {
    if (!draft?.id) { setDraft(null); return; }
    if (!confirm(`Delete "${draft.name}" and its run history?`)) return;
    await api.workflowDelete(draft.id);
    setDraft(null);
    setOpenId(null);
    setDirty(false);
    await qc.invalidateQueries({ queryKey: ["workflows"] });
  });

  const askAi = () => {
    if (dirty && !confirm("Discard your unsaved changes?")) return;
    setView("builder");
    setDraft(blank());
    setOpenId(null);
    setDirty(false);
    setSelected("trigger");
    setDry(null);
    setError(null);
    setPanel("assistant");
  };

  const dryById = useMemo(() => new Map((dry?.steps ?? []).map((s) => [s.id, s])), [dry]);
  const issuesAt = (at: string | null) => issues.filter((i) => i.at === at);
  const errors = issues.filter((i) => i.error).length;

  return (
    <div className="wf">
      <aside className="wf-list">
        <header className="wf-list-head">
          <h2>Workflows</h2>
          <span className="wf-list-actions">
            <Button size="sm" icon="sparkles" onClick={askAi} title="Describe a workflow and the assistant builds it">Ask AI</Button>
            <Button size="sm" icon="plus" onClick={() => open(blank())}>New</Button>
          </span>
        </header>
        <div className="wf-list-items">
          <button className={cx("wf-list-item", view === "runs" && "active")} onClick={() => setView("runs")}>
            <Icon name="list" size={14} className="subtle" />
            <span className="wf-list-text">
              <span className="wf-list-name">All runs</span>
              <span className="subtle">{runsSummary(allRuns)}</span>
            </span>
          </button>
          <div className="wf-list-sep" />
          {list.map((w) => {
            const last = lastRun.get(w.id);
            return (
              <button key={w.id} className={cx("wf-list-item", view === "builder" && draft?.id === w.id && "active")} onClick={() => (draft?.id === w.id ? setView("builder") : open(w))}>
                <span className={cx("wf-dot", w.enabled && "on")} title={w.enabled ? "On" : "Off"} />
                <span className="wf-list-text">
                  <span className="wf-list-name">{w.name}</span>
                  <span className="subtle">{describeTrigger(w.trigger, connectors)}</span>
                  {last && <span className={cx("subtle", "wf-list-last", `tone-${runTone[last.status]}`)}>{runLabel[last.status]} {timeAgo(last.started_at)}</span>}
                </span>
                {w.created_by === "agent" && <Icon name="sparkles" size={12} className="subtle" />}
              </button>
            );
          })}
          {!list.length && <p className="subtle wf-empty-note">No workflows yet. Start from a template, describe one to the assistant, or build one.</p>}
        </div>
        <h4 className="label-sm wf-templates-head">Start from a template</h4>
        <div className="wf-templates">
          {templates.map((t) => (
            <button key={t.name} className="wf-template" onClick={() => open({ ...clone(t), id: "", created_by: "template" })}>
              <strong>{t.name}</strong>
              <span className="subtle">{t.description}</span>
            </button>
          ))}
        </div>
      </aside>

      {view === "runs" ? (
        <AllRuns runs={allRuns} workflows={list} onOpenWorkflow={(id) => { const w = list.find((x) => x.id === id); if (w) { if (draft?.id === id) setView("builder"); else open(w); setPanel("runs"); } }} />
      ) : draft ? (
        <>
          <section className="wf-main">
            <header className="wf-bar">
              <input className="wf-name" value={draft.name} onChange={(e) => edit((w) => (w.name = e.target.value))} aria-label="Workflow name" />
              {draft.created_by === "agent" && <Badge tone="accent" icon="sparkles">AI draft</Badge>}
              {dirty ? <Badge tone="warning">Unsaved</Badge> : draft.id && <span className="subtle wf-version">v{draft.version}</span>}
              <span className="wf-bar-spacer" />
              <TestItemPicker value={testItem} onChange={setTestItem} />
              <Button size="sm" icon="eye" onClick={runDry} disabled={!!busy}>{busy === "dry" ? "Running…" : "Dry run"}</Button>
              {draft.trigger.type === "manual" && draft.id && <Button size="sm" icon="zap" onClick={runNow} disabled={!!busy || dirty}>Run now</Button>}
              <Button size="sm" variant={dirty ? "primary" : "secondary"} icon="check" onClick={save} disabled={!dirty || !!busy}>Save</Button>
              <label className={cx("wf-switch", draft.enabled && "on")} title={errors ? "Fix the problems first" : undefined}>
                <Toggle checked={draft.enabled} onChange={(v) => setEnabled(v)} label="On" />
                <span>{draft.enabled ? "On" : "Off"}</span>
              </label>
              <button className="icon-btn" onClick={remove} title="Delete workflow" aria-label="Delete workflow"><Icon name="trash" size={14} /></button>
            </header>
            {error && <div className="wf-error"><Icon name="alert" size={13} /> {error}</div>}
            {dry && <DryRunBanner run={dry} onClose={() => setDry(null)} />}

            <div className="wf-canvas" onClick={(e) => e.target === e.currentTarget && setSelected("trigger")}>
              <div className="wf-flow">
                <TriggerNode workflow={draft} connectors={connectors} selected={selected === "trigger"} issues={issuesAt("trigger")} onClick={() => { setSelected("trigger"); setPanel("edit"); }} />
                {draft.steps.map((s, i) => (
                  <div key={s.id + i} className="wf-flow-item">
                    <Link onAdd={(kind) => edit((w) => insertStep(w, i, kind, connectors, setSelected))} connectors={connectors} workflow={draft} />
                    <StepNode
                      step={s}
                      connectors={connectors}
                      selected={selected === s.id}
                      issues={issuesAt(s.id)}
                      result={dryById.get(s.id)}
                      unreached={!!dry && !dryById.has(s.id)}
                      onClick={() => { setSelected(s.id); setPanel("edit"); }}
                    />
                  </div>
                ))}
                <Link end onAdd={(kind) => edit((w) => insertStep(w, w.steps.length, kind, connectors, setSelected))} connectors={connectors} workflow={draft} />
                {!draft.steps.length && (
                  <div className="wf-canvas-hint">
                    <p className="subtle">Add steps with the + above, or describe what you want and the assistant builds it here.</p>
                    {panel !== "assistant" && <Button size="sm" icon="sparkles" onClick={() => setPanel("assistant")}>Ask the assistant</Button>}
                  </div>
                )}
              </div>
              {issuesAt(null).length > 0 && (
                <div className="wf-issues">
                  {issuesAt(null).map((i, n) => <IssueLine key={n} issue={i} />)}
                </div>
              )}
            </div>
          </section>

          <aside className="wf-side">
            <div className="wf-side-tabs" role="tablist">
              {(["edit", "runs", "assistant"] as const).map((p) => (
                <button key={p} role="tab" aria-selected={panel === p} className={cx("pane-tab", panel === p && "active")} onClick={() => setPanel(p)}>
                  {p === "edit" ? (selected === "trigger" ? "Trigger" : "Step") : p === "runs" ? "Runs" : <><Icon name="sparkles" size={12} /> Assistant</>}
                </button>
              ))}
            </div>
            <div className="wf-side-body">
              {panel === "edit" && (selected === "trigger" || !draft.steps.some((s) => s.id === selected)) && (
                <TriggerEditor workflow={draft} connectors={connectors} edit={edit} issues={issuesAt("trigger")} onAllowDestructive={(v) => draft.id && !dirty ? setEnabled(draft.enabled, v) : edit((w) => (w.allow_destructive = v))}
                  onAllowCommands={(v) => draft.id && !dirty ? setEnabled(draft.enabled, undefined, v) : edit((w) => (w.allow_commands = v))} />
              )}
              {panel === "edit" && selected !== "trigger" && draft.steps.some((s) => s.id === selected) && (
                <StepEditor
                  workflow={draft}
                  index={draft.steps.findIndex((s) => s.id === selected)}
                  connectors={connectors}
                  edit={edit}
                  issues={issuesAt(selected)}
                  onSelect={setSelected}
                />
              )}
              {panel === "runs" && <RunsPanel workflow={draft} />}
              {panel === "assistant" && <AssistantPanel workflow={draft} />}
            </div>
          </aside>
        </>
      ) : (
        <section className="wf-main wf-blank">
          <Icon name="workflow" size={30} />
          <h3>Automate the busywork</h3>
          <p className="subtle">A workflow starts when something happens (a review request, a CI failure, a newsletter, a time of day) and runs steps your connections offer: comment, label, snooze, create a task, signal other workflows.</p>
          <div className="wf-blank-actions">
            <Button variant="primary" icon="plus" onClick={() => open(blank())}>New workflow</Button>
            <Button icon="sparkles" onClick={askAi}>Describe it to the assistant</Button>
          </div>
        </section>
      )}
    </div>
  );
}

type NewStep = { type: "action"; connector: string; action: string } | { type: "filter" | "set" | "emit" | "command" };

function insertStep(w: Workflow, at: number, kind: NewStep, connectors: ConnectorInfo[], select: (id: string) => void) {
  const base = { label: "", when: [] as Condition[], continue_on_error: false };
  let step: Step;
  if (kind.type === "action") {
    const info = findAction(connectors, kind.connector, kind.action);
    const input: Record<string, unknown> = {};
    for (const f of info?.inputs ?? []) if (f.default != null) input[f.key] = f.default;
    step = { ...base, id: uniqueId(w, kind.action.split(".").pop() ?? "step"), type: "action", connector: kind.connector, action: kind.action, input };
  } else if (kind.type === "filter") step = { ...base, id: uniqueId(w, "only_if"), type: "filter", conditions: [{ field: "item.title", op: "contains", value: "" }] };
  else if (kind.type === "set") step = { ...base, id: uniqueId(w, "values"), type: "set", values: { name: "" } };
  else if (kind.type === "command") step = { ...base, id: uniqueId(w, "run"), type: "command", program: "", args: [], folder: "", network: [], env: {}, image: "", timeout_secs: 0 };
  else step = { ...base, id: uniqueId(w, "signal"), type: "emit", signal: "", payload: {} };
  w.steps.splice(at, 0, step);
  setTimeout(() => select(step.id));
}

// ---------------------------------------------------------------------------
// Canvas
// ---------------------------------------------------------------------------

function IssueLine({ issue }: { issue: Issue }) {
  return (
    <div className={cx("wf-issue", issue.error ? "error" : "warn")}>
      <Icon name={issue.error ? "alert" : "dot"} size={12} />
      <span>{issue.message}</span>
    </div>
  );
}

function NodeIssues({ issues }: { issues: Issue[] }) {
  if (!issues.length) return null;
  const errors = issues.filter((i) => i.error).length;
  return <span className={cx("wf-node-issues", errors ? "error" : "warn")} title={issues.map((i) => i.message).join("\n")}>{errors || issues.length}</span>;
}

function TriggerNode({ workflow, connectors, selected, issues, onClick }: { workflow: Workflow; connectors: ConnectorInfo[]; selected: boolean; issues: Issue[]; onClick: () => void }) {
  const t = workflow.trigger;
  const src = t.type === "item" && t.sources.length === 1 ? connectors.find((c) => c.id === t.sources[0]) : undefined;
  const chips: string[] = [];
  if (t.type === "item") {
    for (const k of t.kinds) chips.push(connectors.flatMap((c) => c.kinds).find((x) => x.kind === k)?.label ?? k);
    for (const c of t.when) chips.push(`${c.field.replace(/^item\./, "")} ${ops.find((o) => o.value === c.op)?.label} ${text(c.value)}`.trim());
  }
  if (t.type === "signal") for (const c of t.when) chips.push(`${c.field} ${c.op} ${text(c.value)}`);
  return (
    <button className={cx("wf-node", "wf-node-trigger", selected && "selected")} onClick={onClick}>
      <div className="wf-node-head">
        <SourceIcon icon={t.type === "schedule" ? "clock" : t.type === "signal" ? "zap" : t.type === "manual" ? "user" : src?.icon ?? "inbox"} tone="accent" size={26} />
        <div className="wf-node-title">
          <span className="wf-node-kicker">Trigger</span>
          <strong>{describeTrigger(t, connectors)}</strong>
        </div>
        <NodeIssues issues={issues} />
      </div>
      {chips.length > 0 && <div className="wf-node-chips">{chips.map((c, i) => <span key={i} className="wf-chip">{c}</span>)}</div>}
    </button>
  );
}

function StepNode({ step, connectors, selected, issues, result, unreached, onClick }: { step: Step; connectors: ConnectorInfo[]; selected: boolean; issues: Issue[]; result?: StepRun; unreached?: boolean; onClick: () => void }) {
  let icon = "dot", tone: Tone = "neutral", kicker = "", title = "", lines: string[] = [], effect: Effect | null = null;
  if (step.type === "action") {
    const c = connectors.find((x) => x.id === step.connector);
    const info = findAction(connectors, step.connector, step.action);
    icon = info?.icon ?? "zap";
    effect = info?.effect ?? null;
    tone = effect === "destructive" ? "danger" : effect === "write" ? "info" : "neutral";
    kicker = c?.name ?? step.connector;
    title = step.label || info?.label || step.action;
    lines = (info?.inputs ?? []).filter((f) => text(step.input[f.key])).slice(0, 3).map((f) => `${f.label}: ${text(step.input[f.key])}`);
  } else if (step.type === "filter") {
    icon = "filter"; kicker = "Filter"; title = step.label || "Only continue if";
    lines = step.conditions.map((c) => `${c.field} ${ops.find((o) => o.value === c.op)?.label} ${ops.find((o) => o.value === c.op)?.noValue ? "" : text(c.value)}`);
  } else if (step.type === "set") {
    icon = "edit"; kicker = "Values"; title = step.label || "Set values";
    lines = Object.entries(step.values).map(([k, v]) => `${k} = ${text(v)}`);
  } else if (step.type === "command") {
    icon = "terminal"; tone = "info"; kicker = step.image ? `Command · ${step.image}` : "Command";
    title = step.label || [step.program, ...step.args].join(" ").trim() || "Run a command";
    lines = [step.network.length ? `Network: ${step.network.join(", ")}` : "No network", step.folder ? `In ${step.folder}` : "In a fresh folder"];
  } else {
    icon = "zap"; tone = "warning"; kicker = "Signal"; title = step.label || `Signal ${step.signal || "…"}`;
    lines = Object.entries(step.payload).map(([k, v]) => `${k}: ${text(v)}`);
  }
  return (
    <button className={cx("wf-node", selected && "selected", result && `ran-${result.status}`, unreached && "unreached")} onClick={onClick}>
      <div className="wf-node-head">
        <SourceIcon icon={icon} tone={tone} size={26} />
        <div className="wf-node-title">
          <span className="wf-node-kicker">{kicker}{step.when.length > 0 && " · conditional"}</span>
          <strong>{title}</strong>
        </div>
        {effect && effect !== "read" && <Badge tone={effectTone[effect]}>{effectLabel[effect]}</Badge>}
        <NodeIssues issues={issues} />
      </div>
      {lines.length > 0 && <div className="wf-node-lines">{lines.map((l, i) => <span key={i}>{l}</span>)}</div>}
      {result && (
        <div className="wf-node-result">
          <Badge tone={stepTone[result.status]}>{stepLabel[result.status]}</Badge>
          {result.error && <span className="wf-node-error">{result.error}</span>}
        </div>
      )}
      <span className="wf-node-id">{step.id}</span>
    </button>
  );
}

/** The line between nodes, with a + that adds a step there. */
function Link({ onAdd, end, connectors, workflow }: { onAdd: (k: NewStep) => void; end?: boolean; connectors: ConnectorInfo[]; workflow: Workflow }) {
  const [open, setOpen] = useState(false);
  return (
    <div className={cx("wf-link", end && "end")}>
      <span className="wf-link-line" />
      <span className="menu-anchor">
        <button className={cx("wf-add", end && "big")} onClick={() => setOpen((v) => !v)} aria-label="Add a step" title="Add a step">
          <Icon name="plus" size={end ? 14 : 12} />{end && <span>Add step</span>}
        </button>
        {open && <AddMenu connectors={connectors} workflow={workflow} onPick={(k) => { setOpen(false); onAdd(k); }} onClose={() => setOpen(false)} />}
      </span>
    </div>
  );
}

function AddMenu({ connectors, workflow, onPick, onClose }: { connectors: ConnectorInfo[]; workflow: Workflow; onPick: (k: NewStep) => void; onClose: () => void }) {
  const ref = useRef<HTMLDivElement>(null);
  const [q, setQ] = useState("");
  useEffect(() => {
    const away = (e: MouseEvent) => ref.current && !ref.current.contains(e.target as Node) && onClose();
    const esc = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("mousedown", away);
    window.addEventListener("keydown", esc);
    return () => { window.removeEventListener("mousedown", away); window.removeEventListener("keydown", esc); };
  }, [onClose]);
  // Actions that fit the trigger's kinds come first.
  const kinds = workflow.trigger.type === "item" ? workflow.trigger.kinds : [];
  const fits = (a: ActionInfo) => !a.for_kinds?.length || !kinds.length || a.for_kinds.some((p) => kinds.some((k) => (p.endsWith(".*") ? k.startsWith(p.slice(0, -1)) : k === p)));
  const groups = connectors
    .filter((c) => c.actions?.length)
    .map((c) => ({ c, actions: c.actions!.filter((a) => !q || `${a.label} ${a.description} ${c.name}`.toLowerCase().includes(q.toLowerCase())).sort((a, b) => Number(fits(b)) - Number(fits(a))) }))
    .filter((g) => g.actions.length);
  return (
    <div className="wf-add-menu" ref={ref}>
      <input autoFocus placeholder="Find an action…" value={q} onChange={(e) => setQ(e.target.value)} />
      <div className="wf-add-scroll">
        {!q && (
          <div className="wf-add-group">
            <span className="label-sm">Logic</span>
            <button onClick={() => onPick({ type: "filter" })}><Icon name="filter" size={14} /> <span><strong>Only continue if</strong><small>Stop the run unless conditions hold</small></span></button>
            <button onClick={() => onPick({ type: "set" })}><Icon name="edit" size={14} /> <span><strong>Set values</strong><small>Compute text for later steps</small></span></button>
            <button onClick={() => onPick({ type: "emit" })}><Icon name="zap" size={14} /> <span><strong>Signal other workflows</strong><small>Start workflows listening for it</small></span></button>
            <button onClick={() => onPick({ type: "command" })}><Icon name="terminal" size={14} /> <span><strong>Run a command</strong><small>A program in tab's sandbox, or in a container</small></span></button>
          </div>
        )}
        {groups.map(({ c, actions }) => (
          <div key={c.id} className="wf-add-group">
            <span className="label-sm">{c.name}{!c.enabled && " · not installed"}</span>
            {actions.map((a) => (
              <button key={a.id} className={cx(!fits(a) && "dim")} onClick={() => onPick({ type: "action", connector: c.id, action: a.id })}>
                <Icon name={a.icon} size={14} />
                <span><strong>{a.label}</strong><small>{a.description}</small></span>
                {a.effect !== "read" && <Badge tone={effectTone[a.effect]}>{effectLabel[a.effect]}</Badge>}
              </button>
            ))}
          </div>
        ))}
      </div>
    </div>
  );
}

function DryRunBanner({ run, onClose }: { run: WorkflowRun; onClose: () => void }) {
  const wouldRun = run.steps.filter((s) => s.status === "dry").length;
  return (
    <div className={cx("wf-dry", `tone-${runTone[run.status]}`)}>
      <Icon name="eye" size={14} />
      <span>
        <strong>Dry run {run.status === "succeeded" ? "finished" : runLabel[run.status].toLowerCase()}</strong> {run.trigger.replace(/^by hand/, "")}
        {" · "}{wouldRun ? `${wouldRun} write step${wouldRun === 1 ? "" : "s"} would run` : "nothing would change"}
        {run.error && <> · {run.error}</>}
      </span>
      <button className="icon-btn" onClick={onClose} aria-label="Clear dry run"><Icon name="x" size={13} /></button>
    </div>
  );
}

function TestItemPicker({ value, onChange }: { value: string; onChange: (id: string) => void }) {
  const { data: items = [] } = useItems("open", "");
  return (
    <select className="wf-select wf-test-item" value={value} onChange={(e) => onChange(e.target.value)} title="The item dry runs use">
      <option value="">Test on: no item</option>
      {items.slice(0, 60).map((i) => <option key={i.id} value={i.id}>{i.title.slice(0, 70)}</option>)}
    </select>
  );
}

// ---------------------------------------------------------------------------
// Inspector
// ---------------------------------------------------------------------------

type Edit = (f: (w: Workflow) => void) => void;

function Field({ label, help, children }: { label: string; help?: string; children: React.ReactNode }) {
  return (
    <label className="wf-field">
      <span className="wf-field-label">{label}</span>
      {children}
      {help && <small className="subtle">{help}</small>}
    </label>
  );
}

function TriggerEditor({ workflow: w, connectors, edit, issues, onAllowDestructive, onAllowCommands }: { workflow: Workflow; connectors: ConnectorInfo[]; edit: Edit; issues: Issue[]; onAllowDestructive: (v: boolean) => void; onAllowCommands: (v: boolean) => void }) {
  const t = w.trigger;
  const setType = (type: Trigger["type"]) =>
    edit((x) => {
      x.trigger =
        type === "item" ? { type, changes: ["ingested"], sources: [], kinds: [], when: [] }
          : type === "signal" ? { type, name: "", when: [] }
            : type === "schedule" ? { type, every_minutes: 60 } : { type };
    });
  const sources = connectors.filter((c) => c.kinds.length && c.available);
  const kinds = t.type === "item" ? (t.sources.length ? connectors.filter((c) => t.sources.includes(c.id)) : sources).flatMap((c) => c.kinds) : [];
  const hasDestructive = w.steps.some((s) => s.type === "action" && findAction(connectors, s.connector, s.action)?.effect === "destructive");
  return (
    <div className="wf-editor">
      <h3>Trigger</h3>
      {issues.map((i, n) => <IssueLine key={n} issue={i} />)}
      <div className="seg wf-seg">
        {(["item", "schedule", "signal", "manual"] as const).map((k) => (
          <button key={k} className={cx("seg-btn", t.type === k && "active")} onClick={() => t.type !== k && setType(k)}>
            {k === "item" ? "Item" : k === "schedule" ? "Schedule" : k === "signal" ? "Signal" : "By hand"}
          </button>
        ))}
      </div>
      {t.type === "item" && (
        <>
          <Field label="When an item is">
            <div className="wf-chips">
              {changes.map((c) => (
                <button key={c.value} className={cx("kind-chip", !t.changes.includes(c.value) && "off")} onClick={() => edit((x) => {
                  const tr = x.trigger as Extract<Trigger, { type: "item" }>;
                  tr.changes = tr.changes.includes(c.value) ? tr.changes.filter((v) => v !== c.value) : [...tr.changes, c.value];
                })}>{c.label}</button>
              ))}
            </div>
          </Field>
          <Field label="From" help="Narrow triggers keep workflows from feeding each other.">
            <div className="wf-chips">
              {sources.map((c) => (
                <button key={c.id} className={cx("kind-chip", !t.sources.includes(c.id) && "off")} onClick={() => edit((x) => {
                  const tr = x.trigger as Extract<Trigger, { type: "item" }>;
                  tr.sources = tr.sources.includes(c.id) ? tr.sources.filter((v) => v !== c.id) : [...tr.sources, c.id];
                  tr.kinds = tr.kinds.filter((k) => !tr.sources.length || connectors.some((cc) => tr.sources.includes(cc.id) && cc.kinds.some((kk) => kk.kind === k)) || k.includes("*"));
                })}>{c.name}</button>
              ))}
            </div>
          </Field>
          <Field label="Kinds" help="None picked means any kind.">
            <div className="wf-chips">
              {kinds.map((k) => (
                <button key={k.kind} className={cx("kind-chip", !t.kinds.includes(k.kind) && "off")} onClick={() => edit((x) => {
                  const tr = x.trigger as Extract<Trigger, { type: "item" }>;
                  tr.kinds = tr.kinds.includes(k.kind) ? tr.kinds.filter((v) => v !== k.kind) : [...tr.kinds, k.kind];
                })}><Icon name={k.icon} size={11} /> {k.label}</button>
              ))}
            </div>
          </Field>
          <Field label="Only when">
            <Conditions value={t.when} vars={variables(w, 0, connectors)} onChange={(c) => edit((x) => ((x.trigger as Extract<Trigger, { type: "item" }>).when = c))} />
          </Field>
        </>
      )}
      {t.type === "schedule" && (
        <Field label="Every" help="At least every 5 minutes. A day starts at midnight UTC.">
          <div className="wf-row">
            <input className="wf-input" type="number" min={5} value={t.every_minutes} onChange={(e) => edit((x) => ((x.trigger as Extract<Trigger, { type: "schedule" }>).every_minutes = Number(e.target.value)))} />
            <span className="subtle">minutes</span>
            {[15, 60, 240, 1440].map((m) => <button key={m} className="kind-chip" onClick={() => edit((x) => ((x.trigger as Extract<Trigger, { type: "schedule" }>).every_minutes = m))}>{m < 60 ? `${m}m` : m < 1440 ? `${m / 60}h` : "1 day"}</button>)}
          </div>
        </Field>
      )}
      {t.type === "signal" && (
        <>
          <Field label="Signal name" help="Another workflow's Signal step with this name starts this one. Its payload is {{signal.<key>}}.">
            <input className="wf-input" value={t.name} placeholder="ci_failed" onChange={(e) => edit((x) => ((x.trigger as Extract<Trigger, { type: "signal" }>).name = e.target.value))} />
          </Field>
          <Field label="Only when">
            <Conditions value={t.when} vars={variables(w, 0, connectors)} onChange={(c) => edit((x) => ((x.trigger as Extract<Trigger, { type: "signal" }>).when = c))} />
          </Field>
        </>
      )}
      {t.type === "manual" && <p className="subtle">Runs only when you press Run now, on the item picked in the toolbar.</p>}

      <h3 className="wf-section">About</h3>
      <Field label="Description">
        <textarea className="textarea wf-textarea" rows={2} value={w.description} onChange={(e) => edit((x) => (x.description = e.target.value))} />
      </Field>

      <h3 className="wf-section">Limits and safety</h3>
      <div className="wf-row2">
        <Field label="Runs per hour" help="Triggers past this are skipped.">
          <input className="wf-input" type="number" min={1} value={w.limits.max_runs_per_hour} onChange={(e) => edit((x) => (x.limits.max_runs_per_hour = Math.max(1, Number(e.target.value))))} />
        </Field>
        <Field label="Cool-down (min)" help="Per item. 0 is none.">
          <input className="wf-input" type="number" min={0} value={w.limits.cooldown_minutes} onChange={(e) => edit((x) => (x.limits.cooldown_minutes = Math.max(0, Number(e.target.value))))} />
        </Field>
      </div>
      <div className={cx("wf-safety", w.allow_destructive && "on")}>
        <div>
          <strong>Allow destructive steps</strong>
          <p className="subtle">Merging, closing and moving mail stay blocked until you allow them here.{hasDestructive ? "" : " This workflow has none."}</p>
        </div>
        <Toggle checked={w.allow_destructive} onChange={onAllowDestructive} label="Allow destructive steps" />
      </div>
      <div className={cx("wf-safety", w.allow_commands && "on")}>
        <div>
          <strong>Allow commands</strong>
          <p className="subtle">Command steps run programs on this computer, in tab's sandbox: only their folder, the hosts they list, and none of your credentials.{w.steps.some((s) => s.type === "command") ? "" : " This workflow has none."}</p>
        </div>
        <Toggle checked={!!w.allow_commands} onChange={onAllowCommands} label="Allow commands" />
      </div>
      <p className="subtle wf-guard-note">
        <Icon name="shield" size={12} /> A workflow never re-runs from its own changes, chains stop after three workflows, the same trigger runs once, and actions wait when a connection's rate budget is tight.
      </p>
    </div>
  );
}

type CommandStep = Extract<Step, { type: "command" }>;

/** A command step: what runs, where, and what it may reach. */
function CommandFields({ step, vars, set }: { step: CommandStep; vars: { path: string; label: string }[]; set: (f: (s: Step) => void) => void }) {
  const cmd = (f: (s: CommandStep) => void) => set((s) => s.type === "command" && f(s));
  return (
    <>
      <Field label="Program" help="Run directly, never through a shell. A name on PATH, or a path.">
        <input className="wf-input mono" value={step.program} placeholder="cargo" onChange={(e) => cmd((s) => (s.program = e.target.value))} />
      </Field>
      <Field label="Arguments" help="One per line. Templates like {{item.metadata.repo}} are passed as plain text.">
        <textarea className="textarea wf-textarea mono" rows={3} value={step.args.join("\n")} placeholder={"test\n--workspace"} onChange={(e) => cmd((s) => (s.args = e.target.value.split("\n")))} />
      </Field>
      <Field label="Folder" help="Where it runs and the only place it can change. Empty gives each run a fresh folder, removed afterwards.">
        <input className="wf-input mono" value={step.folder} placeholder="A fresh folder per run" onChange={(e) => cmd((s) => (s.folder = e.target.value))} />
      </Field>
      <Field label="Network" help="Hosts it may reach, one per line. Empty means none; * means any.">
        <textarea className="textarea wf-textarea mono" rows={2} value={step.network.join("\n")} placeholder={"crates.io\n*.crates.io"} onChange={(e) => cmd((s) => (s.network = e.target.value.split("\n")))} />
      </Field>
      <div className="wf-row2">
        <Field label="Container image" help="Optional. Runs in Docker or Podman instead, with only the folder mounted.">
          <input className="wf-input mono" value={step.image} placeholder="rust:1" onChange={(e) => cmd((s) => (s.image = e.target.value.trim()))} />
        </Field>
        <Field label="Time limit (s)" help="0 is 10 minutes; at most an hour.">
          <input className="wf-input" type="number" min={0} max={3600} value={step.timeout_secs} onChange={(e) => cmd((s) => (s.timeout_secs = Math.max(0, Math.min(3600, Math.round(Number(e.target.value) || 0)))))} />
        </Field>
      </div>
      <Field label="Environment" help="Only these, plus PATH and the like. None of tab's tokens are passed on.">
        <Pairs value={step.env} vars={vars} keyLabel="Name" onChange={(v) => cmd((s) => (s.env = v))} />
      </Field>
      <p className="subtle wf-outputs">
        Later steps can use <code>{`{{steps.${step.id}.exit_code}}`}</code><code>{`{{steps.${step.id}.stdout}}`}</code><code>{`{{steps.${step.id}.stderr}}`}</code>
      </p>
    </>
  );
}

function StepEditor({ workflow: w, index, connectors, edit, issues, onSelect }: { workflow: Workflow; index: number; connectors: ConnectorInfo[]; edit: Edit; issues: Issue[]; onSelect: (id: string) => void }) {
  const step = w.steps[index];
  const vars = variables(w, index, connectors);
  const set = (f: (s: Step) => void) => edit((x) => f(x.steps[index]));
  const info = step.type === "action" ? findAction(connectors, step.connector, step.action) : undefined;
  const move = (d: number) => edit((x) => {
    const j = index + d;
    if (j < 0 || j >= x.steps.length) return;
    [x.steps[index], x.steps[j]] = [x.steps[j], x.steps[index]];
  });
  return (
    <div className="wf-editor">
      <div className="wf-editor-head">
        <h3>{step.type === "action" ? info?.label ?? step.action : step.type === "filter" ? "Only continue if" : step.type === "set" ? "Set values" : step.type === "command" ? "Run a command" : "Signal"}</h3>
        <button className="icon-btn" onClick={() => move(-1)} disabled={index === 0} title="Move up" aria-label="Move up"><Icon name="arrow-up" size={13} /></button>
        <button className="icon-btn" onClick={() => move(1)} disabled={index === w.steps.length - 1} title="Move down" aria-label="Move down"><Icon name="arrow-down" size={13} /></button>
        <button className="icon-btn" onClick={() => { edit((x) => x.steps.splice(index, 1)); onSelect("trigger"); }} title="Remove step" aria-label="Remove step"><Icon name="trash" size={13} /></button>
      </div>
      {info && <p className="subtle wf-desc">{info.description}{info.spends && <> Spends from {info.spends.replace("/", " ")} budget.</>}</p>}
      {issues.map((i, n) => <IssueLine key={n} issue={i} />)}

      {step.type === "action" && (
        <>
          <Field label="Action">
            <select className="wf-select" value={`${step.connector}/${step.action}`} onChange={(e) => {
              const [c, a] = e.target.value.split("/");
              const next = findAction(connectors, c, a);
              set((s) => {
                if (s.type !== "action") return;
                s.connector = c;
                s.action = a;
                const input: Record<string, unknown> = {};
                for (const f of next?.inputs ?? []) input[f.key] = s.input[f.key] ?? f.default ?? "";
                s.input = Object.fromEntries(Object.entries(input).filter(([, v]) => v !== ""));
              });
            }}>
              {connectors.filter((c) => c.actions?.length).map((c) => (
                <optgroup key={c.id} label={c.name}>
                  {c.actions!.map((a) => <option key={a.id} value={`${c.id}/${a.id}`}>{a.label}</option>)}
                </optgroup>
              ))}
            </select>
          </Field>
          {(info?.inputs ?? []).map((f) => (
            <InputField key={f.key} field={f} value={step.input[f.key]} vars={vars} onChange={(v) => set((s) => {
              if (s.type !== "action") return;
              if (v === "" || v == null) delete s.input[f.key]; else s.input[f.key] = v;
            })} />
          ))}
          {(info?.outputs.length ?? 0) > 0 && (
            <p className="subtle wf-outputs">
              Later steps can use {info!.outputs.map((o) => <code key={o.key}>{`{{steps.${step.id}.${o.key}}}`}</code>)}
            </p>
          )}
        </>
      )}
      {step.type === "filter" && <Conditions value={step.conditions} vars={vars} onChange={(c) => set((s) => s.type === "filter" && (s.conditions = c))} />}
      {step.type === "set" && <Pairs value={step.values} vars={vars} keyLabel="Name" onChange={(v) => set((s) => s.type === "set" && (s.values = v))} />}
      {step.type === "command" && <CommandFields step={step} vars={vars} set={set} />}
      {step.type === "emit" && (
        <>
          <Field label="Signal name" help="Workflows with a Signal trigger of this name start, with this payload.">
            <input className="wf-input" value={step.signal} placeholder="ci_failed" onChange={(e) => set((s) => s.type === "emit" && (s.signal = e.target.value))} />
          </Field>
          <Field label="Payload"><Pairs value={step.payload} vars={vars} keyLabel="Key" onChange={(v) => set((s) => s.type === "emit" && (s.payload = v))} /></Field>
        </>
      )}

      <h3 className="wf-section">Options</h3>
      <div className="wf-row2">
        <Field label="Step id" help="How later steps refer to it.">
          <input className="wf-input mono" value={step.id} onChange={(e) => {
            const id = e.target.value.replace(/[^a-z0-9_]/gi, "_");
            set((s) => (s.id = id));
            onSelect(id);
          }} />
        </Field>
        <Field label="Label">
          <input className="wf-input" value={step.label} placeholder="Optional" onChange={(e) => set((s) => (s.label = e.target.value))} />
        </Field>
      </div>
      {step.type !== "filter" && (
        <Field label="Run only if" help="Otherwise this step is skipped and the run carries on.">
          <Conditions value={step.when} vars={vars} onChange={(c) => set((s) => (s.when = c))} />
        </Field>
      )}
      {step.type === "action" && (
        <div className="wf-safety">
          <div><strong>Keep going if it fails</strong><p className="subtle">Otherwise a failure ends the run.</p></div>
          <Toggle checked={step.continue_on_error} onChange={(v) => set((s) => (s.continue_on_error = v))} label="Keep going if it fails" />
        </div>
      )}
    </div>
  );
}

/** A text box that takes `{{variables}}`, with a picker to insert one. */
function VarInput({ value, onChange, vars, multiline, placeholder, mono }: { value: string; onChange: (v: string) => void; vars: { path: string; label: string }[]; multiline?: boolean; placeholder?: string; mono?: boolean }) {
  const ref = useRef<HTMLInputElement & HTMLTextAreaElement>(null);
  const [open, setOpen] = useState(false);
  const insert = (path: string) => {
    const el = ref.current;
    const token = `{{${path}}}`;
    const at = el?.selectionStart ?? value.length;
    const end = el?.selectionEnd ?? at;
    onChange(value.slice(0, at) + token + value.slice(end));
    setOpen(false);
    setTimeout(() => { el?.focus(); el?.setSelectionRange(at + token.length, at + token.length); });
  };
  const props = { ref, value, placeholder, onChange: (e: React.ChangeEvent<HTMLInputElement & HTMLTextAreaElement>) => onChange(e.target.value), className: cx(multiline ? "textarea wf-textarea" : "wf-input", mono && "mono") };
  return (
    <div className="wf-var">
      {multiline ? <textarea rows={3} {...props} /> : <input {...props} />}
      <span className="menu-anchor">
        <button type="button" className="wf-var-btn" onClick={() => setOpen((v) => !v)} title="Insert a value from the item or an earlier step">{"{ }"}</button>
        {open && (
          <div className="wf-var-menu" onMouseLeave={() => setOpen(false)}>
            {vars.map((v) => <button key={v.path} onClick={() => insert(v.path)}><span>{v.label}</span><code>{v.path}</code></button>)}
          </div>
        )}
      </span>
    </div>
  );
}

function InputField({ field: f, value, vars, onChange }: { field: ActionField; value: unknown; vars: { path: string; label: string }[]; onChange: (v: unknown) => void }) {
  const v = text(value);
  const templated = v.includes("{{");
  let control: React.ReactNode;
  if (f.type === "select" && !templated) {
    control = (
      <select className="wf-select" value={v} onChange={(e) => onChange(e.target.value)}>
        {!f.required && <option value="">Default</option>}
        {(f.options ?? []).map((o) => <option key={o} value={o}>{o}</option>)}
      </select>
    );
  } else if (f.type === "bool" && !templated) {
    control = (
      <select className="wf-select" value={v} onChange={(e) => onChange(e.target.value === "" ? "" : e.target.value === "true")}>
        <option value="">Default</option><option value="true">Yes</option><option value="false">No</option>
      </select>
    );
  } else control = <VarInput value={v} onChange={onChange} vars={vars} multiline={f.type === "long_text"} placeholder={f.type === "list" ? "a, b, c" : f.default} />;
  return <Field label={`${f.label}${f.required ? "" : " (optional)"}`} help={f.help}>{control}</Field>;
}

function Conditions({ value, onChange, vars }: { value: Condition[]; onChange: (c: Condition[]) => void; vars: { path: string; label: string }[] }) {
  const listId = useMemo(() => `vars-${Math.random().toString(36).slice(2)}`, []);
  const set = (i: number, patch: Partial<Condition>) => onChange(value.map((c, j) => (j === i ? { ...c, ...patch } : c)));
  return (
    <div className="wf-conds">
      <datalist id={listId}>{vars.map((v) => <option key={v.path} value={v.path}>{v.label}</option>)}</datalist>
      {value.map((c, i) => {
        const op = ops.find((o) => o.value === c.op);
        return (
          <div key={i} className="wf-cond">
            <input className="wf-input mono" list={listId} value={c.field} placeholder="item.title" onChange={(e) => set(i, { field: e.target.value })} />
            <select className="wf-select" value={c.op} onChange={(e) => set(i, { op: e.target.value as Op })}>
              {ops.map((o) => <option key={o.value} value={o.value}>{o.label}</option>)}
            </select>
            {!op?.noValue && <input className="wf-input" value={text(c.value)} placeholder="value" onChange={(e) => set(i, { value: e.target.value })} />}
            <button className="icon-btn" onClick={() => onChange(value.filter((_, j) => j !== i))} aria-label="Remove condition"><Icon name="x" size={12} /></button>
          </div>
        );
      })}
      <button className="wf-link-btn" onClick={() => onChange([...value, { field: vars[0]?.path ?? "item.title", op: "contains", value: "" }])}><Icon name="plus" size={12} /> Add condition</button>
      {value.length > 1 && <small className="subtle">All of these must hold.</small>}
    </div>
  );
}

function Pairs({ value, onChange, vars, keyLabel }: { value: Record<string, unknown>; onChange: (v: Record<string, unknown>) => void; vars: { path: string; label: string }[]; keyLabel: string }) {
  const entries = Object.entries(value);
  const set = (i: number, k: string, v: unknown) => onChange(Object.fromEntries(entries.map((e, j) => (j === i ? [k, v] : e))));
  return (
    <div className="wf-conds">
      {entries.map(([k, v], i) => (
        <div key={i} className="wf-pair">
          <input className="wf-input mono" value={k} placeholder={keyLabel} onChange={(e) => set(i, e.target.value.replace(/[^a-z0-9_]/gi, "_"), v)} />
          <VarInput value={text(v)} vars={vars} onChange={(nv) => set(i, k, nv)} />
          <button className="icon-btn" onClick={() => onChange(Object.fromEntries(entries.filter((_, j) => j !== i)))} aria-label="Remove"><Icon name="x" size={12} /></button>
        </div>
      ))}
      <button className="wf-link-btn" onClick={() => onChange({ ...value, [`value_${entries.length + 1}`]: "" })}><Icon name="plus" size={12} /> Add</button>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Runs and the assistant
// ---------------------------------------------------------------------------

function RunsPanel({ workflow }: { workflow: Workflow }) {
  const { data: runs = [] } = useQuery({
    queryKey: ["workflow-runs", workflow.id],
    queryFn: () => api.workflowRuns(workflow.id || null, 50),
    enabled: !!workflow.id,
    staleTime: 0,
  });
  const [open, setOpen] = useState<string | null>(null);
  if (!workflow.id) return <p className="subtle wf-pad">Save the workflow to start keeping its runs. Dry runs show on the canvas.</p>;
  if (!runs.length) return <p className="subtle wf-pad">No runs yet. Each run is kept with every step's inputs and outputs for 30 days.</p>;
  return (
    <div className="wf-runs">
      {runs.map((r) => (
        <div key={r.id} className={cx("wf-run", open === r.id && "open")}>
          <button className="wf-run-head" onClick={() => setOpen(open === r.id ? null : r.id)}>
            <Badge tone={runTone[r.status]}>{runLabel[r.status]}</Badge>
            <span className="wf-run-trigger" title={r.trigger}>{r.trigger}</span>
            <span className="subtle">{timeAgo(r.started_at)}</span>
          </button>
          {open === r.id && <RunDetail run={r} />}
        </div>
      ))}
    </div>
  );
}

function RunDetail({ run: r }: { run: WorkflowRun }) {
  return (
            <div className="wf-run-body">
              {r.error && <IssueLine issue={{ at: null, error: r.status === "failed", message: r.error }} />}
              {r.chain.length > 0 && <p className="subtle">Started by changes from {r.chain.length} earlier workflow{r.chain.length === 1 ? "" : "s"}.</p>}
              {r.steps.map((s) => (
                <div key={s.id} className="wf-run-step">
                  <div className="wf-run-step-head">
                    <Badge tone={stepTone[s.status]}>{stepLabel[s.status]}</Badge>
                    <code>{s.id}</code>
                    <span className="subtle">{s.ms} ms</span>
                  </div>
                  {s.error && <p className="wf-node-error">{s.error}</p>}
                  {s.input != null && <details><summary>Input</summary><pre>{JSON.stringify(s.input, null, 2)}</pre></details>}
                  {s.output != null && Object.keys(s.output as object).length > 0 && <details><summary>Output</summary><pre>{JSON.stringify(s.output, null, 2)}</pre></details>}
                </div>
              ))}
              {r.item_id && <Button size="sm" icon="external" onClick={() => openItem(r.item_id!)}>Open the item and its timeline</Button>}
            </div>
  );
}

function runsSummary(runs: WorkflowRun[]): string {
  const since = Date.now() - 24 * 3600_000;
  const day = runs.filter((r) => !r.dry_run && new Date(r.started_at).getTime() >= since);
  if (!day.length) return runs.length ? `Last ${timeAgo(runs[0].started_at)}` : "None yet";
  const bad = day.filter((r) => r.status === "failed" || r.status === "blocked").length;
  return `${day.length} today${bad ? ` · ${bad} need a look` : ""}`;
}

type RunFilter = "all" | "problems" | "succeeded" | "dry";

function AllRuns({ runs, workflows, onOpenWorkflow }: { runs: WorkflowRun[]; workflows: Workflow[]; onOpenWorkflow: (id: string) => void }) {
  const [filter, setFilter] = useState<RunFilter>("all");
  const [wf, setWf] = useState<string>("");
  const [open, setOpen] = useState<string | null>(null);
  const shown = runs.filter((r) => {
    if (wf && r.workflow_id !== wf) return false;
    if (filter === "dry") return r.dry_run;
    if (r.dry_run) return false;
    if (filter === "problems") return !["succeeded", "stopped", "running"].includes(r.status);
    if (filter === "succeeded") return r.status === "succeeded";
    return true;
  });
  const exists = new Set(workflows.map((w) => w.id));
  return (
    <section className="wf-main wf-allruns">
      <header className="wf-bar">
        <h2 className="wf-allruns-title">All runs</h2>
        <span className="wf-bar-spacer" />
        <div className="wf-seg" role="tablist">
          {(["all", "problems", "succeeded", "dry"] as const).map((f) => (
            <button key={f} role="tab" aria-selected={filter === f} className={cx(filter === f && "active")} onClick={() => setFilter(f)}>
              {f === "all" ? "All" : f === "problems" ? "Problems" : f === "succeeded" ? "Succeeded" : "Dry runs"}
            </button>
          ))}
        </div>
        <select className="wf-select" value={wf} onChange={(e) => setWf(e.target.value)} aria-label="Workflow">
          <option value="">Every workflow</option>
          {workflows.map((w) => <option key={w.id} value={w.id}>{w.name}</option>)}
        </select>
      </header>
      <div className="wf-allruns-body">
        {!shown.length && <p className="subtle wf-pad">{runs.length ? "No runs match." : "No runs yet. When a workflow runs, each run shows here with every step's inputs and outputs, kept for 30 days."}</p>}
        {shown.map((r) => (
          <div key={r.id} className={cx("wf-run", open === r.id && "open")}>
            <button className="wf-run-head wf-allruns-row" onClick={() => setOpen(open === r.id ? null : r.id)}>
              <Badge tone={runTone[r.status]}>{r.dry_run ? "Dry run" : runLabel[r.status]}</Badge>
              <span className="wf-allruns-name">{r.workflow_name}</span>
              <span className="wf-run-trigger subtle" title={r.trigger}>{r.trigger}</span>
              <span className="subtle wf-allruns-steps">{r.steps.length} step{r.steps.length === 1 ? "" : "s"}</span>
              <span className="subtle" title={new Date(r.started_at).toLocaleString()}>{timeAgo(r.started_at)}</span>
            </button>
            {open === r.id && (
              <>
                <RunDetail run={r} />
                {exists.has(r.workflow_id) && <div className="wf-allruns-open"><Button size="sm" icon="workflow" onClick={() => onOpenWorkflow(r.workflow_id)}>Open the workflow</Button></div>}
              </>
            )}
          </div>
        ))}
      </div>
    </section>
  );
}

function AssistantPanel({ workflow }: { workflow: Workflow }) {
  const qc = useQueryClient();
  // One conversation per workflow; a new draft has its own until it's saved.
  const key = ["chat", "workflows", workflow.id || "new"];
  const { data: chat } = useQuery<Chat>({ queryKey: key, queryFn: () => api.workflowChatOpen(workflow.id || null), staleTime: 0, refetchOnMount: "always" });
  // Once the assistant saves a new draft its chat moves to the workflow's id;
  // drop the draft's cached copy so live updates land on the one shown.
  const prevId = useRef(workflow.id);
  useEffect(() => {
    if (!prevId.current && workflow.id) qc.removeQueries({ queryKey: ["chat", "workflows", "new"], exact: true });
    prevId.current = workflow.id;
  }, [qc, workflow.id]);
  const send = async (text: string) => {
    qc.setQueryData<Chat>(key, (c) => ({
      session: c?.session ?? null, approvals: c?.approvals ?? [], running: true,
      messages: [...(c?.messages ?? []), { id: -Date.now(), session_id: c?.session?.id ?? "", role: "user", kind: "text", content: text, meta: null, created_at: new Date().toISOString() }],
    }));
    try {
      const session = await api.workflowChatSend(text, workflow.steps.length || workflow.id ? workflow : null);
      qc.setQueryData<Chat>(key, (c) => ({ session, approvals: c?.approvals ?? [], running: true, messages: (c?.messages ?? []).filter((m) => m.id > 0) }));
    } catch (e) {
      qc.setQueryData<Chat>(key, (c) => (c ? { ...c, running: false, messages: [...c.messages, { id: -Date.now() - 1, session_id: "", role: "system", kind: "error", content: String(e), meta: null, created_at: new Date().toISOString() }] } : c));
    } finally {
      qc.invalidateQueries({ queryKey: key });
    }
  };
  const ideas = workflow.steps.length
    ? ["Dry-run this on a real item and tell me what it would do", "Make this only run for PRs in repos I own", "Add a step that labels the PR too"]
    : ["When my review is requested on a small PR, add an urgent task", "Snooze Dependabot PRs until Friday morning", "When CI fails on my PR, comment with the failing job and make a task"];
  return (
    <div className="wf-assistant">
      {chat?.session && <div className="wf-assistant-status"><AgentStatus chat={chat} /></div>}
      <div className="wf-assistant-transcript">
        <Transcript
          chat={chat}
          onSend={send}
          intro={!chat?.messages.length && (
            <div className="wf-assistant-intro">
              <p>Describe what you want automated. The assistant builds it from your connections' actions, saves it here turned off, and dry-runs it so you can check before turning it on.</p>
              <div className="suggestions">{ideas.map((s) => <button key={s} className="suggestion" onClick={() => send(s)}><Icon name="zap" size={13} /> {s}</button>)}</div>
            </div>
          )}
        />
      </div>
      <Composer chat={chat} onSend={(t) => send(t)} placeholder={workflow.steps.length ? "Ask for a change to this workflow…" : "Describe a workflow…"} />
    </div>
  );
}
