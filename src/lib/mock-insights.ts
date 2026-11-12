// Browser stand-in for workflow insights: thirteen months of made-up runs
// for the mock workflows, and a small copy of `tab_domain::workflow::stats`
// that adds them up, so the Factory and Insights views have a believable floor
// to show (a healthy workflow, a degraded one, a failing one, people
// stepping in, API and subscription spend, versions, several models on a
// step). Windows past 30 days behave like the app's: UTC days, failures and
// models from the last 30 days only, and a year's previous window left
// empty (rollups are kept 13 months). Only the mock uses this; the app gets
// its numbers from Rust.

import type {
  CapUsage, DayStats, FactoryWorkflow, FailureGroup, ModelStats, Quantiles, RunCounts, Spend, StepStats, StepStatus, VersionStats, Workflow, WorkflowFactory,
  WorkflowHealth, WorkflowInsights, WorkflowRun, WorkflowStats,
} from "./types";

type Billing = "api" | "subscription";
type Kind = "approval" | "question" | "message" | "stopped" | "rerun";
interface Metrics {
  queue_ms: number; work_ms: number; human_wait_ms: number; billing: Billing; cost_usd: number; plan_share: number;
  input_tokens: number; output_tokens: number; family: string; harness: string; model: string; attempts: number; fallbacks: number;
}
interface MStep { id: string; label: string; status: StepStatus; error: string | null; ms: number; metrics: Metrics | null; interventions: { kind: Kind; at: string }[]; input: unknown; output: unknown }
type MRun = Omit<WorkflowRun, "steps"> & { steps: MStep[]; rerun_of: string | null };

// ---------------------------------------------------------------------------
// Extra workflows, so the floor has agent work on it
// ---------------------------------------------------------------------------

const base = { when: [], continue_on_error: false };
const agentStep = (id: string, label: string, prompt: string, repo: string) =>
  ({ ...base, id, label, type: "agent" as const, prompt, repo, harness: "", model: "", placement: { type: "anywhere" as const }, deadline_minutes: 45 });

export function extraWorkflows(ago: (min: number) => string): Workflow[] {
  const w = (id: string, name: string, description: string, trigger: Workflow["trigger"], steps: Workflow["steps"], version: number, extra: Partial<Workflow> = {}): Workflow => ({
    id, name, description, enabled: true, allow_destructive: false, trigger, steps, limits: { max_runs_per_hour: 30, cooldown_minutes: 0 },
    created_by: "user", version, updated_at: ago(60 * 24 * 2), ...extra,
  });
  return [
    w("wf-ci", "Fix failing CI on my PRs", "When CI fails on one of my pull requests, an agent fixes it and pushes to the branch.",
      { type: "item", changes: ["ingested"], sources: ["github"], kinds: ["github.ci_failed"], when: [] },
      [
        { ...base, id: "pr", label: "Read the PR", type: "action", connector: "github", action: "pr.get", input: { repo: "{{item.metadata.repo}}", number: "{{item.metadata.number}}" } },
        agentStep("fix", "Fix the failing job", "CI failed on {{item.url}}. Find the failing job, fix it on the PR's branch and push.", "{{item.metadata.repo}}"),
        { ...base, id: "comment", label: "Say what changed", type: "action", connector: "github", action: "issue.comment", input: { repo: "{{item.metadata.repo}}", number: "{{item.metadata.number}}", body: "{{steps.fix.summary}}" } },
      ], 5, { limits: { max_runs_per_hour: 30, cooldown_minutes: 0, monthly_cap_usd: 12, monthly_cap_plan_tokens: 6_000_000 } }),
    w("wf-deps", "Nightly dependency bumps", "Every night: update dependencies, run the tests, and have an agent open a PR with notes.",
      { type: "schedule", every_minutes: 1440 },
      [
        { ...base, id: "bump", label: "Update packages", type: "command", program: "pnpm", args: ["update", "--latest"], folder: "~/src/tab", network: ["registry.npmjs.org"], env: {}, image: "", timeout_secs: 300 },
        { ...base, id: "tests", label: "Run the tests", type: "command", program: "pnpm", args: ["test"], folder: "~/src/tab", network: [], env: {}, image: "", timeout_secs: 600 },
        agentStep("pr", "Open a PR with notes", "Open a pull request with the bumped packages and a note per breaking change.", "vincentcosta6/tab"),
      ], 3, { allow_commands: true, limits: { max_runs_per_hour: 30, cooldown_minutes: 0, monthly_cap_usd: 2, monthly_cap_plan_tokens: 0 } }),
    w("wf-merge", "Merge green Dependabot PRs", "When a Dependabot PR is green, review the diff and merge it after you OK it.",
      { type: "item", changes: ["ingested"], sources: ["github"], kinds: ["github.review_requested"], when: [{ field: "item.actor", op: "equals", value: "dependabot[bot]" }] },
      [
        { ...base, id: "pr", label: "Read the PR", type: "action", connector: "github", action: "pr.get", input: { repo: "{{item.metadata.repo}}", number: "{{item.metadata.number}}" } },
        agentStep("review", "Review the changelog", "Read the changelog of the bumped package and say if anything breaks us.", "{{item.metadata.repo}}"),
        { ...base, id: "merge", label: "Merge", type: "action", connector: "github", action: "pr.merge", input: { repo: "{{item.metadata.repo}}", number: "{{item.metadata.number}}" } },
      ], 2, { allow_destructive: true }),
    w("wf-stale", "Close stale issues", "Every Monday, close issues nobody has touched in 90 days.", { type: "schedule", every_minutes: 10080 },
      [{ ...base, id: "close", label: "Close", type: "action", connector: "github", action: "issue.close", input: { repo: "vincentcosta6/tab", number: "{{item.metadata.number}}" } }], 1, { enabled: false }),
  ];
}

// ---------------------------------------------------------------------------
// Made-up runs
// ---------------------------------------------------------------------------

function rng(seed: number) {
  return () => {
    seed |= 0; seed = (seed + 0x6d2b79f5) | 0;
    let t = Math.imul(seed ^ (seed >>> 15), 1 | seed);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const DAY = 86_400_000;
/** Days of made-up history: past the 13 months a year's view can reach. */
const HISTORY_DAYS = 400;
const MODELS = {
  sonnet: { harness: "claude_code", model: "claude-sonnet-4-5", family: "balanced", billing: "subscription" as Billing, perK: 0 },
  opus: { harness: "claude_code", model: "claude-opus-4-1", family: "balanced", billing: "subscription" as Billing, perK: 0 },
  codex: { harness: "codex", model: "gpt-5-codex", family: "", billing: "api" as Billing, perK: 0.0042 },
  haiku: { harness: "claude_code", model: "claude-haiku-4-5", family: "fast", billing: "api" as Billing, perK: 0.0011 },
};

function generate(workflows: Workflow[], now: number): MRun[] {
  const r = rng(7);
  const pick = <T,>(xs: T[]) => xs[Math.floor(r() * xs.length)];
  const between = (a: number, b: number) => a + (b - a) * r();
  const out: MRun[] = [];
  const name = (id: string) => workflows.find((w) => w.id === id)?.name ?? id;
  let n = 0;
  const run = (wf: string, version: number, at: number, steps: MStep[], status: MRun["status"], extra: Partial<MRun> = {}): MRun => {
    const ms = steps.reduce((s, x) => s + (x.metrics ? x.metrics.queue_ms + x.metrics.work_ms : x.ms), 0);
    const triggers: Record<string, string> = {
      "wf-0": "email.newsletter ingested: This week in Rust", "wf-1": "github.review_requested ingested: Bump tokio to 1.48",
      "wf-2": "github.ci_failed ingested: Faster cold start", "wf-ci": "github.ci_failed ingested: Page the queue and chat",
      "wf-deps": "Every day at 02:00", "wf-merge": "github.review_requested ingested: Bump vite from 8.0.15 to 8.0.16",
    };
    const r: MRun = {
      id: `run-${wf}-${(n++).toString(36)}`, workflow_id: wf, workflow_name: name(wf), version, status, dry_run: false, trigger: triggers[wf] ?? "by hand", trigger_key: null,
      item_id: null, chain: [], steps, error: steps.find((s) => s.error)?.error ?? null, started_at: new Date(at).toISOString(),
      finished_at: new Date(at + ms + 400).toISOString(), rerun_of: null, ...extra,
    };
    return r;
  };
  const plain = (id: string, label: string, status: StepStatus, ms: number, error: string | null = null): MStep =>
    ({ id, label, status, error, ms: Math.round(ms), metrics: null, interventions: [], input: null, output: null });
  const agent = (id: string, label: string, status: StepStatus, m: keyof typeof MODELS, work: number, human = 0, error: string | null = null, fallbacks = 0): MStep => {
    const x = MODELS[m];
    const tokensIn = Math.round(between(18_000, 90_000) * (work / 300_000));
    const tokensOut = Math.round(tokensIn * between(0.06, 0.12));
    return {
      id, label, status, error, ms: 40, interventions: [], input: null, output: null,
      metrics: {
        queue_ms: Math.round(between(4_000, 70_000)), work_ms: Math.round(work), human_wait_ms: Math.round(human), billing: x.billing,
        cost_usd: x.billing === "api" ? +(tokensIn / 1000 * x.perK + tokensOut / 1000 * x.perK * 4).toFixed(4) : 0,
        plan_share: x.billing === "subscription" ? +(tokensIn / 2_400_000).toFixed(4) : 0, input_tokens: tokensIn, output_tokens: tokensOut,
        family: x.family, harness: x.harness, model: x.model, attempts: 1 + fallbacks, fallbacks,
      },
    };
  };
  const ask = (s: MStep, kind: Kind, at: number) => { s.interventions.push({ kind, at: new Date(at).toISOString() }); return s; };

  for (let d = HISTORY_DAYS - 1; d >= 0; d--) {
    const dayStart = now - d * DAY;
    // Quieter at weekends and in late December; busier as more repos joined.
    const wd = new Date(dayStart).getUTCDay();
    const md = new Date(dayStart).toISOString().slice(5, 10);
    const load = (wd === 0 || wd === 6 ? 0.45 : 1) * (md >= "12-22" || md <= "01-02" ? 0.35 : 1) * (d > 200 ? 0.7 : 1);
    const many = (a: number, b: number) => Math.round(between(a, b) * load);
    const at = () => Math.min(now - 60_000, dayStart - between(0, 0.95) * DAY);
    // Snooze newsletters: steady, quick, never fails.
    for (let i = many(6, 12); i > 0; i--)
      out.push(run("wf-0", 1, at(), [plain("snooze", "Snooze until morning", "ok", between(80, 260))], "succeeded"));
    // Small PRs get a review task: mostly fine; a filter stops big ones.
    for (let i = many(2, 5); i > 0; i--) {
      const t = at(), p = r();
      if (p < (d > 120 && d < 160 ? 0.2 : 0.015)) out.push(run("wf-1", 2, t, [plain("pr", "", "failed", between(800, 2400), `GitHub API returned 502 for ${pick(["tab", "hub", "site"])}#${Math.round(between(100, 999))}`)], "failed"));
      else if (p < 0.4) out.push(run("wf-1", 2, t, [plain("pr", "", "ok", between(300, 900)), plain("small", "", "stopped", 2)], "stopped"));
      else out.push(run("wf-1", 2, t, [plain("pr", "", "ok", between(300, 900)), plain("small", "", "ok", 2), plain("task", "", "ok", between(20, 60))], "succeeded"));
    }
    // Fix failing CI: an agent, often asked a question; v5 (5 days ago) moved to Sonnet and got better.
    const ciVersion = d >= 150 ? 1 : d >= 60 ? 2 : d >= 12 ? 3 : d >= 5 ? 4 : 5;
    for (let i = many(2, 4); i > 0; i--) {
      const t = at(), p = r();
      const model: keyof typeof MODELS = ciVersion === 5 ? (r() < 0.75 ? "sonnet" : "codex") : ciVersion === 4 ? pick(["codex", "codex", "opus"]) : ciVersion <= 2 ? pick(["codex", "opus"]) : "opus";
      const okRate = ciVersion === 5 ? (model === "sonnet" ? 0.84 : 0.7) : ciVersion === 4 ? 0.6 : ciVersion === 1 ? 0.5 : ciVersion === 2 ? 0.62 : 0.72;
      const pr = plain("pr", "Read the PR", "ok", between(300, 900));
      if (p > okRate) {
        const err = pick([
          ["Job deadline passed after 45 minutes", "fix"],
          ["Job deadline passed after 45 minutes", "fix"],
          [`Tests still fail: 3 of ${Math.round(between(200, 400))} failed in crates/tab-core`, "fix"],
          ["The runner went away (tab cloud runner restarted)", "fix"],
        ]);
        const fix = agent("fix", "Fix the failing job", "failed", model, between(900_000, 2_700_000), 0, err[0], r() < 0.2 ? 1 : 0);
        out.push(run("wf-ci", ciVersion, t, [pr, fix], "failed"));
      } else {
        const asked = r() < (ciVersion === 5 ? 0.22 : 0.4);
        const fix = agent("fix", "Fix the failing job", "ok", model, between(240_000, 1_500_000), asked ? between(120_000, 900_000) : 0, null, r() < 0.08 ? 1 : 0);
        if (asked) ask(fix, r() < 0.7 ? "question" : "message", t + 120_000);
        const comment = plain("comment", "Say what changed", "ok", between(200, 600));
        const rerun = r() < 0.08 ? "run-wf-ci-prev" : null;
        out.push(run("wf-ci", ciVersion, t, [pr, fix, comment], "succeeded", { rerun_of: rerun }));
      }
    }
    // Nightly dependency bumps: broke a week ago when a package went private.
    {
      const t = dayStart - between(0.1, 0.2) * DAY;
      const roll = [r(), r(), r(), r(), r()];
      if (t < now) {
        const broken = d <= 13 ? roll[0] < 0.9 : roll[0] < 0.1;
        const bump = plain("bump", "Update packages", broken && roll[1] < 0.35 ? "failed" : "ok", 30_000 + roll[2] * 65_000,
          null);
        if (bump.status === "failed") { bump.error = `npm ERR! 404 Not Found - GET https://registry.npmjs.org/@acme%2fui-kit - Not found (${Math.round(1000 + roll[3] * 8999)})`; out.push(run("wf-deps", 3, t, [bump], "failed")); }
        else {
          const tests = plain("tests", "Run the tests", broken ? "failed" : "ok", 140_000 + roll[3] * 120_000, broken ? `Exit code 1: Error: Cannot find module 'vite/internal' from src/main.tsx:${Math.round(1 + roll[4] * 39)}` : null);
          if (broken) out.push(run("wf-deps", 3, t, [bump, tests], "failed"));
          else out.push(run("wf-deps", 3, t, [bump, tests, agent("pr", "Open a PR with notes", "ok", "haiku", 120_000 + roll[4] * 180_000)], "succeeded"));
        }
      }
    }
    // Merge green Dependabot PRs: always asks for the OK to merge, by design.
    for (let i = many(0, 3); i > 0; i--) {
      const t = at();
      const review = agent("review", "Review the changelog", "ok", r() < 0.5 ? "haiku" : "sonnet", between(60_000, 200_000));
      const merge = ask(plain("merge", "Merge", "ok", between(400, 1200)), "approval", t + 200_000);
      merge.ms += Math.round(between(600_000, 7_200_000)); // the wait for the OK
      if (d > 240) continue; // added eight months ago
      out.push(run("wf-merge", d > 20 ? 1 : 2, t, [plain("pr", "Read the PR", "ok", between(300, 800)), review, merge], r() < 0.02 ? "failed" : "succeeded"));
    }
  }
  // Label PRs that fail CI: turned on only recently.
  out.push(run("wf-2", 3, now - 0.4 * DAY, [plain("label", "", "ok", 420), plain("tell", "", "ok", 3)], "succeeded"));
  out.push(run("wf-2", 3, now - 1.3 * DAY, [plain("label", "", "failed", 2100, "Label ci-failed does not exist in tab")], "failed"));
  // A few that did no work, so in-progress and held-back counts show.
  out.push(run("wf-ci", 5, now - 25 * 60_000, [plain("pr", "Read the PR", "ok", 400), { ...agent("fix", "Fix the failing job", "waiting", "sonnet", 0), metrics: null }], "waiting", { finished_at: null }));
  out.push(run("wf-1", 2, now - 3 * 3600_000, [], "throttled"));
  return out.sort((a, b) => b.started_at.localeCompare(a.started_at));
}

// ---------------------------------------------------------------------------
// A small copy of tab_domain::workflow::stats::summarize
// ---------------------------------------------------------------------------

const zeroSpend = (): Spend => ({ cost_usd: 0, input_tokens: 0, output_tokens: 0, plan_input_tokens: 0, plan_output_tokens: 0, plan_share: 0 });
const zeroCounts = (): RunCounts => ({ runs: 0, succeeded: 0, unattended: 0, failed: 0, attended: 0, unplanned: 0 });
function quantiles(v: number[]): Quantiles {
  if (!v.length) return { n: 0, p50: 0, p95: 0 };
  const s = [...v].sort((a, b) => a - b);
  const at = (q: number) => s[Math.min(s.length, Math.max(1, Math.ceil(q * s.length))) - 1];
  return { n: s.length, p50: at(0.5), p95: at(0.95) };
}
function addSpend(s: Spend, m: Metrics | null) {
  if (!m) return;
  s.input_tokens += m.input_tokens; s.output_tokens += m.output_tokens;
  if (m.billing === "subscription") { s.plan_input_tokens += m.input_tokens; s.plan_output_tokens += m.output_tokens; s.plan_share += m.plan_share; }
  else s.cost_usd += m.cost_usd;
}
const didWork = (r: MRun) => !r.dry_run && ["succeeded", "stopped", "failed", "blocked"].includes(r.status);
const succeeded = (r: MRun) => r.status === "succeeded" || r.status === "stopped";
function attendance(r: MRun): boolean | null {
  const kinds: Kind[] = r.steps.flatMap((s) => s.interventions.map((i) => i.kind));
  if (r.status === "blocked" && !kinds.length) kinds.push("approval");
  if (r.rerun_of) kinds.push("rerun");
  return kinds.length ? kinds.every((k) => k === "approval") : null;
}
function addCounts(c: RunCounts, r: MRun) {
  c.runs++;
  const a = attendance(r);
  if (succeeded(r)) { c.succeeded++; if (a === null) c.unattended++; } else c.failed++;
  if (a !== null) { c.attended++; if (!a) c.unplanned++; }
}
const runMs = (r: MRun) => (r.finished_at ? Math.max(0, Date.parse(r.finished_at) - Date.parse(r.started_at)) : null);
const stepMs = (s: MStep) => (s.metrics && s.metrics.queue_ms + s.metrics.work_ms > 0 ? s.metrics.queue_ms + s.metrics.work_ms : s.ms);
const normalize = (e: string) => e.replace(/\b[0-9a-f]{7,}\b/gi, (m) => (/\d/.test(m) ? "#" : m)).replace(/\d+/g, "#").replace(/\s+/g, " ").trim().slice(0, 160);

type Group = { counts: RunCounts; durations: number[]; spend: Spend; first: string | null };
const group = (): Group => ({ counts: zeroCounts(), durations: [], spend: zeroSpend(), first: null });
function addGroup(g: Group, r: MRun) {
  addCounts(g.counts, r);
  const ms = runMs(r); if (ms != null) g.durations.push(ms);
  for (const s of r.steps) addSpend(g.spend, s.metrics);
  if (!g.first || r.started_at < g.first) g.first = r.started_at;
}

function summarize(runs: MRun[], from: number, to: number, offsetMin: number): WorkflowStats {
  const all = group();
  const versions = new Map<number, Group>(), days = new Map<string, Group>();
  const steps: (StepStats & { a: number[]; q: number[]; w: number[]; h: number[] })[] = [];
  const models: (ModelStats & { w: number[] })[] = [];
  const failures: FailureGroup[] = [];
  let in_progress = 0, held_back = 0, loop_stopped = 0;
  const sorted = runs.filter((r) => !r.dry_run && Date.parse(r.started_at) >= from && Date.parse(r.started_at) < to)
    .sort((a, b) => b.started_at.localeCompare(a.started_at));
  for (const run of sorted) {
    if (run.status === "running" || run.status === "waiting") in_progress++;
    else if (run.status === "throttled") held_back++;
    else if (run.status === "loop_stopped") loop_stopped++;
    if (!didWork(run)) continue;
    addGroup(all, run);
    if (!versions.has(run.version)) versions.set(run.version, group());
    addGroup(versions.get(run.version)!, run);
    const day = new Date(Date.parse(run.started_at) + offsetMin * 60_000).toISOString().slice(0, 10);
    if (!days.has(day)) days.set(day, group());
    addGroup(days.get(day)!, run);
    for (const sr of run.steps) {
      let s = steps.find((x) => x.id === sr.id);
      if (!s) {
        const q = quantiles([]);
        s = { id: sr.id, label: sr.label, runs: 0, ok: 0, failed: 0, blocked: 0, throttled: 0, skipped: 0, ms: q, queue_ms: q, work_ms: q, human_wait_ms: q, spend: zeroSpend(), interventions: 0, unplanned_interventions: 0, fallbacks: 0, a: [], q: [], w: [], h: [] };
        steps.push(s);
      }
      s.interventions += sr.interventions.length;
      s.unplanned_interventions += sr.interventions.filter((i) => i.kind !== "approval").length;
      if (sr.status === "skipped") { s.skipped++; continue; }
      if (sr.status === "dry" || sr.status === "waiting") continue;
      if (sr.status === "ok" || sr.status === "stopped") s.ok++;
      else if (sr.status === "failed") s.failed++;
      else if (sr.status === "blocked") s.blocked++;
      else s.throttled++;
      s.runs++;
      addSpend(s.spend, sr.metrics);
      s.a.push(stepMs(sr));
      if (sr.status === "failed") {
        const error = normalize(sr.error ?? "Failed");
        const f = failures.find((x) => x.step_id === sr.id && x.error === error);
        if (f) { f.count++; if (run.started_at < f.first_seen) f.first_seen = run.started_at; }
        else failures.push({ step_id: sr.id, label: sr.label, error, count: 1, first_seen: run.started_at, last_seen: run.started_at, sample_run: run.id });
      }
      const m = sr.metrics;
      if (!m) continue;
      s.fallbacks += m.fallbacks;
      s.q.push(m.queue_ms); s.w.push(m.work_ms); s.h.push(m.human_wait_ms);
      let x = models.find((y) => y.step_id === sr.id && y.harness === m.harness && y.model === m.model);
      if (!x) { x = { step_id: sr.id, harness: m.harness, model: m.model, family: m.family, runs: 0, ok: 0, failed: 0, unattended: 0, work_ms: quantiles([]), spend: zeroSpend(), w: [] }; models.push(x); }
      x.runs++;
      if (sr.status === "failed") x.failed++;
      else { x.ok++; if (!sr.interventions.length) x.unattended++; }
      addSpend(x.spend, m);
      x.w.push(m.work_ms);
    }
  }
  const versionList: VersionStats[] = [...versions.entries()].sort((a, b) => b[0] - a[0])
    .map(([version, g]) => ({ version, ...g.counts, duration_ms: quantiles(g.durations), spend: g.spend, first_run: g.first }));
  const dayList: DayStats[] = [...days.entries()].sort((a, b) => a[0].localeCompare(b[0]))
    .map(([day, g]) => ({ day, ...g.counts, duration_ms: quantiles(g.durations), spend: g.spend }));
  return {
    ...all.counts, in_progress, held_back, loop_stopped, duration_ms: quantiles(all.durations), spend: all.spend,
    steps: steps.map(({ a, q, w, h, ...s }) => ({ ...s, ms: quantiles(a), queue_ms: quantiles(q), work_ms: quantiles(w), human_wait_ms: quantiles(h) })),
    models: models.map(({ w, ...m }) => ({ ...m, work_ms: quantiles(w) })).sort((a, b) => b.spend.cost_usd - a.spend.cost_usd || b.runs - a.runs),
    versions: versionList,
    failures: failures.sort((a, b) => b.count - a.count || b.last_seen.localeCompare(a.last_seen)),
    days: dayList,
  };
}

function health(runs: MRun[]): WorkflowHealth {
  const latest = runs.filter(didWork).sort((a, b) => b.started_at.localeCompare(a.started_at)).slice(0, 20);
  if (latest.length < 3) return "unknown";
  const ok = latest.filter(succeeded).length / latest.length;
  return ok >= 0.9 ? "healthy" : ok >= 0.5 ? "degraded" : "failing";
}

function windows(days: number, offsetMin: number) {
  days = Math.min(365, Math.max(1, Math.round(days)));
  // Past 30 days, windows break at UTC midnight, like the app's.
  if (days > 30) offsetMin = 0;
  const now = Date.now();
  const local = new Date(now + offsetMin * 60_000);
  const midnightLocal = Date.UTC(local.getUTCFullYear(), local.getUTCMonth(), local.getUTCDate()) - (days - 1) * DAY;
  const from = midnightLocal - offsetMin * 60_000;
  const today = Date.UTC(new Date(now).getUTCFullYear(), new Date(now).getUTCMonth(), new Date(now).getUTCDate());
  const keepFrom = new Date(today); keepFrom.setUTCMonth(keepFrom.getUTCMonth() - 13);
  const split = today - 30 * DAY;
  return { days, offsetMin, from, to: from + days * DAY, prevFrom: from - days * DAY, split, long: days > 30, keepFrom: keepFrom.getTime() };
}

/** A long window: everything from runs (the mock keeps them all), failures and models from the run log only. */
function windowStats(runs: MRun[], w: ReturnType<typeof windows>, from: number, to: number): WorkflowStats {
  if (!w.long) return summarize(runs, from, to, w.offsetMin);
  if (from < w.keepFrom) return summarize([], from, to, 0);
  const s = summarize(runs, from, to, 0);
  if (from >= w.split) return s;
  const recent = to > w.split ? summarize(runs, w.split, to, 0) : summarize([], 0, 0, 0);
  return { ...s, models: recent.models, failures: recent.failures };
}

function capUsage(wf: Workflow | undefined, runs: MRun[]): CapUsage | null {
  const usd = wf?.limits.monthly_cap_usd ?? 0, tok = wf?.limits.monthly_cap_plan_tokens ?? 0;
  if (!usd && !tok) return null;
  const n = new Date();
  const month = Date.UTC(n.getUTCFullYear(), n.getUTCMonth(), 1);
  const spend = zeroSpend();
  for (const r of runs) if (!r.dry_run && Date.parse(r.started_at) >= month) for (const s of r.steps) addSpend(spend, s.metrics);
  return { month: new Date(month).toISOString().slice(0, 10), spend, cap_usd: usd, cap_plan_tokens: tok };
}

const runLevel = (s: WorkflowStats): WorkflowStats => ({ ...s, steps: [], models: [], versions: [], failures: [] });
const rank: Record<WorkflowHealth, number> = { failing: 0, degraded: 1, healthy: 2, unknown: 3 };

// ---------------------------------------------------------------------------
// The mock commands
// ---------------------------------------------------------------------------

let cache: { runs: MRun[]; at: number } | null = null;
function runsFor(workflows: Workflow[]): MRun[] {
  if (!cache || Date.now() - cache.at > 10 * 60_000) cache = { runs: generate(workflows, Date.now()), at: Date.now() };
  return cache.runs;
}

/** The made-up history, newest first, for the Runs views: the 30 days the run log keeps. */
export function insightRuns(workflows: Workflow[]): WorkflowRun[] {
  const from = new Date(Date.now() - 30 * DAY).toISOString();
  return runsFor(workflows).filter((r) => r.started_at >= from) as unknown as WorkflowRun[];
}

export function insightsMock(cmd: string, args: Record<string, any>, workflows: Workflow[]): unknown {
  if (cmd !== "workflow_insights" && cmd !== "workflow_factory") return undefined;
  const runs = runsFor(workflows);
  const w = windows(args.days ?? 7, args.utcOffsetMinutes ?? 0);
  const iso = (t: number) => new Date(t).toISOString();
  const one = (id: string) => {
    const mine = runs.filter((r) => r.workflow_id === id);
    return { health: health(mine), stats: windowStats(mine, w, w.from, w.to), previous: windowStats(mine, w, w.prevFrom, w.from) };
  };
  const detail_from = w.long && w.from < w.split ? iso(w.split) : null;
  if (cmd === "workflow_insights") {
    const out: WorkflowInsights = {
      workflow_id: args.workflowId, days: w.days, from: iso(w.from), to: iso(w.to), ...one(args.workflowId), detail_from,
      cap: capUsage(workflows.find((x) => x.id === args.workflowId), runs.filter((r) => r.workflow_id === args.workflowId)),
    };
    return out;
  }
  const cards: FactoryWorkflow[] = workflows.map((x) => ({ id: x.id, name: x.name, enabled: x.enabled, version: x.version, ...one(x.id) }));
  const rate = (s: WorkflowStats) => (s.runs ? s.unattended / s.runs : 1);
  cards.sort((a, b) => rank[a.health] - rank[b.health] || rate(a.stats) - rate(b.stats) || b.stats.runs - a.stats.runs || a.name.localeCompare(b.name));
  const listed = runs.filter((r) => workflows.some((x) => x.id === r.workflow_id));
  const out: WorkflowFactory = {
    days: w.days, from: iso(w.from), to: iso(w.to),
    totals: runLevel(windowStats(listed, w, w.from, w.to)),
    previous_totals: runLevel(windowStats(listed, w, w.prevFrom, w.from)),
    workflows: cards, detail_from,
  };
  return out;
}
