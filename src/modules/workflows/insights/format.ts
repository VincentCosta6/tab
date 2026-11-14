// Numbers for the Insights and Factory views: whole percentages, humane
// durations, dollars to the cent, tokens as k/M. Subscription usage never
// gets a dollar figure.

import type { RunCounts, Spend, Tone, WorkflowHealth } from "../../../lib/types";

/** Fewer runs than this and a rate says nothing (the same as `HEALTH_MIN_RUNS`). */
export const MIN_RUNS = 3;

export const rate = (n: number, of: number): number | null => (of > 0 ? n / of : null);
export const pct = (r: number | null | undefined) => (r == null ? "–" : `${Math.round(r * 100)}%`);

export function duration(ms: number): string {
  if (ms < 1000) return ms < 50 ? `${Math.round(ms)}ms` : `${(ms / 1000).toFixed(1)}s`;
  const s = Math.round(ms / 1000);
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60), rs = s % 60;
  if (m < 10) return rs ? `${m}m ${rs}s` : `${m}m`;
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60), rm = m % 60;
  return rm ? `${h}h ${rm}m` : `${h}h`;
}

export const usd = (n: number) => (n > 0 && n < 0.01 ? "<$0.01" : `$${n.toFixed(2)}`);

export function tokens(n: number): string {
  if (n < 1000) return `${n}`;
  if (n < 1_000_000) return `${(n / 1000).toFixed(n < 10_000 ? 1 : 0)}k`;
  return `${(n / 1_000_000).toFixed(n < 10_000_000 ? 1 : 0)}M`;
}

/** Plan allowance as a share of one usage window ("35% of a window", "2.4 windows"). */
export const planShare = (share: number) => (share < 1 ? `${Math.max(1, Math.round(share * 100))}% of a window` : `${share.toFixed(1)} windows`);

export const hasApi = (s: Spend) => s.cost_usd > 0;
export const hasPlan = (s: Spend) => s.plan_input_tokens + s.plan_output_tokens > 0;
export const planTokens = (s: Spend) => s.plan_input_tokens + s.plan_output_tokens;

/** The short form: dollars for API keys, plan tokens for subscriptions, both when both. */
export function spendParts(s: Spend, compact = false): string[] {
  const parts: string[] = [];
  if (hasApi(s)) parts.push(usd(s.cost_usd));
  if (hasPlan(s)) parts.push(`${tokens(planTokens(s))} plan ${compact ? "tok" : "tokens"}`);
  return parts;
}

export function spendShort(s: Spend, compact = false): string {
  const parts = spendParts(s, compact);
  return parts.length ? parts.join(" + ") : "Nothing";
}

/** Each way it was paid for, spelled out (for titles). */
export function spendLong(s: Spend): string {
  const parts: string[] = [];
  if (hasApi(s)) parts.push(`${usd(s.cost_usd)} billed through API keys`);
  if (hasPlan(s)) parts.push(`${tokens(planTokens(s))} tokens on a subscription (${planShare(s.plan_share)} of plan allowance)`);
  return parts.length ? parts.join("; ") : "No agent spend";
}

export const healthTone: Record<WorkflowHealth, Tone> = { healthy: "success", degraded: "warning", failing: "danger", unknown: "neutral" };
export const healthLabel: Record<WorkflowHealth, string> = { healthy: "Healthy", degraded: "Degraded", failing: "Failing", unknown: "Too new to say" };
export const healthIcon: Record<WorkflowHealth, string> = { healthy: "check", degraded: "alert", failing: "octagon", unknown: "clock" };

/** The same thresholds as `health`, for a rate on its own. */
export function rateTone(r: number | null, runs: number): Tone {
  if (r == null || runs < MIN_RUNS) return "neutral";
  return r >= 0.9 ? "success" : r >= 0.5 ? "warning" : "danger";
}

export const successRate = (c: RunCounts) => rate(c.succeeded, c.runs);
export const unattendedRate = (c: RunCounts) => rate(c.unattended, c.runs);
export const interventionRate = (c: RunCounts) => rate(c.attended, c.runs);

/** The window's days as local `YYYY-MM-DD`, oldest first, so empty days still get a slot. */
export function windowDays(from: string, days: number): string[] {
  const offset = -new Date().getTimezoneOffset() * 60_000;
  const start = Date.parse(from) + offset;
  return Array.from({ length: days }, (_, i) => new Date(start + i * 86_400_000).toISOString().slice(0, 10));
}

/** The local day an instant falls on, as `YYYY-MM-DD`. */
export const localDay = (iso: string) => new Date(Date.parse(iso) - new Date().getTimezoneOffset() * 60_000).toISOString().slice(0, 10);

export function dayLabel(day: string, long = false): string {
  const d = new Date(`${day}T12:00:00`);
  return d.toLocaleDateString(undefined, long ? { weekday: "short", month: "short", day: "numeric" } : { month: "short", day: "numeric" });
}

export const plural = (n: number, one: string, many = `${one}s`) => `${n} ${n === 1 ? one : many}`;
