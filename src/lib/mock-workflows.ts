// Browser stand-in for workflows. Action catalogs and templates come from
// the Rust registry (see examples/workflow_catalog.rs), so the builder is
// designed against the real metadata.

import catalog from "./mock-workflows.json";
import type { ActionInfo, Activity, Chat, Condition, ConnectorInfo, EngineEvent, Issue, KindInfo, QueueItem, Session, StepRun, Workflow, WorkflowRun } from "./types";

export const NOT_HANDLED = Symbol("not handled");

type Catalog = { connectors: { id: string; name: string; icon: string; kinds: KindInfo[]; actions: ActionInfo[] }[]; templates: Workflow[] };
const cat = catalog as unknown as Catalog;

/** Adds actions to the mock connectors, and the email connector. */
export function withActions(connectors: ConnectorInfo[]): ConnectorInfo[] {
  const out = connectors.map((c) => ({ ...c, actions: cat.connectors.find((x) => x.id === c.id)?.actions ?? [] }));
  const email = cat.connectors.find((c) => c.id === "email");
  if (email && !out.some((c) => c.id === "email"))
    out.splice(2, 0, {
      id: "email", name: email.name, icon: email.icon, description: "Mail from any IMAP host.", kinds: email.kinds, actions: email.actions,
      settings: [], available: true, rate_limits: [], category: "communication", features: [], builtin: false, enabled: true,
      status: { state: "running", last_sync: new Date().toISOString() },
    });
  return out;
}

const ago = (min: number) => new Date(Date.now() - min * 60_000).toISOString();
const clone = <T,>(v: T): T => JSON.parse(JSON.stringify(v));

let workflows: Workflow[] = cat.templates.slice(0, 3).map((t, i) => ({
  ...clone(t), id: `wf-${i}`, enabled: i !== 2, version: i + 1, created_by: i === 1 ? "agent" : "user", updated_at: ago(60 * (i + 2)),
}));

function lookup(ctx: unknown, path: string): unknown {
  let cur: any = ctx;
  for (const p of path.trim().split(".").filter(Boolean)) {
    if (cur == null) return undefined;
    cur = cur[p];
  }
  return cur;
}
function render(v: unknown, ctx: unknown): unknown {
  if (typeof v === "string") {
    const whole = v.match(/^\{\{([^}]+)\}\}$/);
    if (whole) return lookup(ctx, whole[1]) ?? null;
    return v.replace(/\{\{([^}]+)\}\}/g, (_, p) => {
      const x = lookup(ctx, p);
      return x == null ? "" : Array.isArray(x) ? x.join(", ") : String(x);
    });
  }
  if (Array.isArray(v)) return v.map((x) => render(x, ctx));
  if (v && typeof v === "object") return Object.fromEntries(Object.entries(v).map(([k, x]) => [k, render(x, ctx)]));
  return v;
}
function holds(c: Condition, ctx: unknown): boolean {
  const v = lookup(ctx, c.field);
  const want = render(c.value, ctx);
  const t = String(v ?? "").toLowerCase(), w = String(want ?? "").toLowerCase();
  switch (c.op) {
    case "equals": return t === w;
    case "not_equals": return t !== w;
    case "contains": return Array.isArray(v) ? v.some((x) => String(x).toLowerCase() === w) : t.includes(w);
    case "not_contains": return !t.includes(w);
    case "starts_with": return t.startsWith(w);
    case "matches": return new RegExp(`^${w.split("*").map((s) => s.replace(/[.+?^${}()|[\]\\]/g, "\\$&")).join(".*")}$`).test(t);
    case "in": return w.split(",").map((s) => s.trim()).includes(t);
    case "gt": return Number(v) > Number(want);
    case "lt": return Number(v) < Number(want);
    case "exists": return v != null && t !== "";
    case "missing": return v == null || t === "";
  }
}

function findAction(connectors: ConnectorInfo[], c: string, a: string) {
  return connectors.find((x) => x.id === c)?.actions?.find((x) => x.id === a);
}

function validate(w: Workflow, connectors: ConnectorInfo[]): Issue[] {
  const out: Issue[] = [];
  if (!w.name.trim()) out.push({ at: null, error: true, message: "Give the workflow a name." });
  if (w.trigger.type === "item" && !w.trigger.changes.length) out.push({ at: "trigger", error: true, message: "Pick at least one change to start on, such as a new item." });
  if (w.trigger.type === "item" && !w.trigger.sources.length && !w.trigger.kinds.length && !w.trigger.when.length)
    out.push({ at: "trigger", error: false, message: "Runs for every item from every connection. Narrow it to a connection or kind if you can." });
  if (w.trigger.type === "schedule" && w.trigger.every_minutes < 5) out.push({ at: "trigger", error: true, message: "Schedules run at most every 5 minutes." });
  if (!w.steps.length) out.push({ at: null, error: false, message: "Add a step for the workflow to do something." });
  const seen = new Set<string>();
  for (const s of w.steps) {
    if (s.type === "action") {
      const info = findAction(connectors, s.connector, s.action);
      if (!info) out.push({ at: s.id, error: true, message: `${s.connector} has no action ${s.action}.` });
      else {
        for (const f of info.inputs.filter((f) => f.required))
          if (String(s.input[f.key] ?? "").trim() === "") out.push({ at: s.id, error: true, message: `${info.label}: ${f.label} is required.` });
        if (info.effect === "destructive" && !w.allow_destructive)
          out.push({ at: s.id, error: false, message: `${info.label} is destructive, so it stays blocked until you allow destructive steps.` });
      }
      for (const v of Object.values(s.input))
        for (const m of String(v ?? "").matchAll(/\{\{\s*steps\.([a-z0-9_]+)/gi))
          if (!seen.has(m[1])) out.push({ at: s.id, error: true, message: `{{steps.${m[1]}…}} reads a step that hasn't run yet.` });
    }
    seen.add(s.id);
  }
  // The real check walks every enabled workflow; this catches the common one.
  const createsTask = w.steps.some((s) => s.type === "action" && s.connector === "tab" && s.action === "task.create");
  if (createsTask && w.trigger.type === "item" && w.trigger.changes.includes("ingested") && (!w.trigger.sources.length || w.trigger.sources.includes("tab")))
    out.push({ at: null, error: true, message: `This could loop: ${w.name} → ${w.name}. Narrow a trigger (for example to other connections) or drop the step that feeds it.` });
  return out;
}

let runs: WorkflowRun[] = [];

function execute(w: Workflow, item: QueueItem | null, dry: boolean, connectors: ConnectorInfo[]): WorkflowRun {
  const ctx: any = { item: item ? { ...item, score: item.ranking?.score, urgency: item.ranking?.urgency, metadata: { repo: item.context, number: Number(item.url?.split("/pull/")[1]?.split("/")[0]) || 482, folder: "INBOX", uid: 4211, ...(item.metadata as object) } } : null, steps: {}, trigger: { type: "manual" }, now: new Date().toISOString() };
  const steps: StepRun[] = [];
  let status: WorkflowRun["status"] = "succeeded";
  for (const s of w.steps) {
    const sr: StepRun = { id: s.id, label: s.label, status: "ok", input: null, output: null, error: null, ms: Math.round(Math.random() * 300) };
    if (!s.when.every((c) => holds(c, ctx))) { sr.status = "skipped"; steps.push(sr); continue; }
    if (s.type === "filter") {
      const failed = s.conditions.filter((c) => !holds(c, ctx));
      if (failed.length) { sr.status = "stopped"; sr.output = { failed: failed.map((c) => `${c.field} ${c.op} ${JSON.stringify(c.value)} (was ${JSON.stringify(lookup(ctx, c.field) ?? null)})`) }; status = "stopped"; }
    } else if (s.type === "set") sr.output = render(s.values, ctx);
    else if (s.type === "emit") { sr.input = { signal: s.signal, payload: render(s.payload, ctx) }; if (dry) sr.status = "dry"; }
    else if (s.type === "command") {
      sr.input = { program: s.program, args: s.args, folder: s.folder, network: s.network, image: s.image };
      if (dry) sr.status = "dry";
      else if (!w.allow_commands) { sr.status = "blocked"; sr.error = "Commands are blocked. Allow commands on this workflow to let it run programs."; status = "blocked"; }
      else sr.output = { exit_code: 0, stdout: "test result: ok. 42 passed\n", stderr: "", sandbox: "seatbelt" };
    }
    else {
      sr.input = render(s.input, ctx);
      const info = findAction(connectors, s.connector, s.action);
      if (info?.effect === "destructive" && !w.allow_destructive && !dry) { sr.status = "blocked"; sr.error = `${info.label} is destructive. Allow destructive steps on this workflow to let it run.`; status = "blocked"; }
      else if (dry && info?.effect !== "read") { sr.status = "dry"; sr.output = Object.fromEntries((info?.outputs ?? []).map((f) => [f.key, `<${f.label}>`])); }
      else sr.output = s.action === "pr.get" ? { state: "open", draft: false, author: "dana-k", base: "main", head: "idempotency", additions: 612, deletions: 203, labels: ["payments"], url: item?.url } : { item_id: "mock-new" };
    }
    ctx.steps[s.id] = sr.output ?? {};
    steps.push(sr);
    if (sr.status === "stopped" || sr.status === "blocked") break;
  }
  return {
    id: `run-${Date.now()}-${Math.random().toString(36).slice(2, 6)}`, workflow_id: w.id, workflow_name: w.name, version: w.version, status, dry_run: dry,
    trigger: item ? `by hand on ${item.title}` : "by hand", trigger_key: null, item_id: item?.id ?? null, chain: [], steps, error: steps.find((s) => s.error)?.error ?? null,
    started_at: new Date().toISOString(), finished_at: new Date().toISOString(),
  };
}

// One builder chat per workflow, like the app ("new" for an unsaved draft).
const chats = new Map<string, Chat>();
const chatFor = (id: string | null | undefined): Chat => {
  const k = id || "new";
  if (!chats.has(k)) chats.set(k, { session: null, messages: [], approvals: [], running: false });
  return chats.get(k)!;
};

export function workflowMock(cmd: string, args: Record<string, any>, env: { items: () => QueueItem[]; connectors: () => ConnectorInfo[]; emit: (e: EngineEvent) => void }): unknown {
  const connectors = env.connectors();
  if (!runs.length) {
    // A little history so the Runs view has something to show.
    const items = env.items();
    const ci = items.find((i) => i.kind === "github.review_requested");
    const nl = items.find((i) => i.kind === "email.newsletter");
    if (nl) runs.push({ ...execute(workflows[0], nl, false, connectors), started_at: ago(42), finished_at: ago(42), trigger: `email.newsletter ingested: ${nl.title}` });
    if (ci) runs.push({ ...execute(workflows[1], ci, false, connectors), started_at: ago(35), finished_at: ago(35), trigger: `github.review_requested ingested: ${ci.title}` });
  }
  switch (cmd) {
    case "workflows_list": return clone(workflows);
    case "workflow_templates": return clone(cat.templates);
    case "workflow_validate": return validate(args.workflow, connectors);
    case "workflow_save": {
      const issues = validate(args.workflow, connectors);
      const w: Workflow = { ...clone(args.workflow), updated_at: new Date().toISOString() };
      if (issues.some((i) => i.error)) w.enabled = false;
      if (!w.id) { w.id = `wf-${Date.now()}`; w.version = 1; w.created_by ||= "user"; workflows.push(w); }
      else { w.version += 1; workflows = workflows.map((x) => (x.id === w.id ? w : x)); }
      env.emit({ type: "workflows_changed", id: w.id, by: "user" });
      return { workflow: w, issues };
    }
    case "workflow_set_enabled": {
      const w = workflows.find((x) => x.id === args.id);
      if (!w) throw "no such workflow";
      if (args.enabled) { const e = validate(w, connectors).find((i) => i.error); if (e) throw e.message; }
      w.enabled = args.enabled;
      if (args.allowDestructive != null) w.allow_destructive = args.allowDestructive;
      if (args.allowCommands != null) w.allow_commands = args.allowCommands;
      env.emit({ type: "workflows_changed", id: w.id, by: "user" });
      return clone(w);
    }
    case "workflow_delete": workflows = workflows.filter((w) => w.id !== args.id); return null;
    case "workflow_runs": return runs.filter((r) => !args.workflowId || r.workflow_id === args.workflowId).sort((a, b) => b.started_at.localeCompare(a.started_at)).slice(0, args.limit ?? 50);
    case "workflow_run": {
      const item = env.items().find((i) => i.id === args.itemId) ?? null;
      const run = execute(args.workflow, item, args.dryRun, connectors);
      if (!args.dryRun) runs.unshift(run);
      env.emit({ type: "workflow_run", run });
      return run;
    }
    case "workflow_chat_open": return clone(chatFor(args.workflowId));
    case "workflow_chat_send": {
      const now = new Date().toISOString();
      const chat = chatFor(args.open?.id);
      const session: Session = chat.session ?? { id: `wf-chat-${args.open?.id || "new"}`, item_id: null, agent: "claude_code", agent_session_id: null, title: "Workflow builder", status: "idle", cwd: "~", created_at: now, updated_at: now };
      const id = chat.messages.length + 1;
      chats.set(args.open?.id || "new", {
        session, approvals: [], running: false,
        messages: [
          ...chat.messages,
          { id, session_id: session.id, role: "user", kind: "text", content: args.text, meta: null, created_at: now },
          { id: id + 1, session_id: session.id, role: "assistant", kind: "text", content: "In the desktop app I'd build this with your connections' actions, save it turned off, and dry-run it on a real item so you can check it before turning it on.", meta: null, created_at: now },
        ],
      });
      return session;
    }
    case "item_activity": {
      const item = env.items().find((i) => i.id === args.id);
      if (!item) return [];
      const a = (min: number, kind: string, title: string, by: string | null, via: string | null = null): Activity => ({ id: min, item_id: item.id, at: ago(min), kind, title, by, detail: {}, via });
      const out = [a(Math.max(1, (Date.now() - Date.parse(item.created_at)) / 60_000), "arrived", item.source === "tab" ? "Created" : `Arrived from ${item.source}`, item.actor)];
      if (item.kind.startsWith("github.review")) out.push(a(30, "workflow_run", 'Workflow "Small PRs get a review task" stopped at a filter', "workflow:Small PRs get a review task"));
      if (item.kind.startsWith("agent.") || item.kind === "github.review_requested") {
        out.push(a(20, "session_started", "Claude Code session started", "agent"));
        out.push(a(14, "linked", "Add idempotency keys to payment webhooks (#482)", "agent"));
        out.push(a(6, "finished_upstream", "Pull request merged or closed", "upstream", "Add idempotency keys to payment webhooks (#482)"));
      }
      if (item.state === "snoozed") out.push(a(2, "snoozed", "Snoozed", "you"));
      return out.sort((x, y) => x.at.localeCompare(y.at));
    }
  }
  return NOT_HANDLED;
}
