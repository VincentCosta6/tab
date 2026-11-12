import { useEffect, useRef, useState } from "react";
import { cx } from "../../design/components";
import type { SessionMessage } from "../../lib/types";

export const k = (n: number) => (n >= 10_000 ? `${Math.round(n / 1000)}k` : n >= 1000 ? `${(n / 1000).toFixed(1)}k` : `${n}`);

/** Claude's default prompt-cache lifetime, refreshed on every call. */
const CACHE_TTL = 5 * 60_000;
const clock = (ms: number) => (ms < 60_000 ? "<1m" : `${Math.ceil(ms / 60_000)}m`);
const cacheText = (c: string, left: number) => (c === "none" ? "not used" : c === "cold" ? "expired" : `warm for ${clock(left)}`);

function useNow(every: number) {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const t = window.setInterval(() => setNow(Date.now()), every);
    return () => window.clearInterval(t);
  }, [every]);
  return now;
}

type Usage = NonNullable<ReturnType<typeof contextUsage>>;

/**
 * How full the agent's context window is, from the last turn's usage, with
 * a breakdown: the agent's own setup (system prompt, tools, MCP), what tab
 * adds (this item and its attached context), and the conversation so far.
 * Null before the first turn has finished.
 */
function contextUsage(messages: SessionMessage[], now: number) {
  const turns = messages.filter((m) => m.kind === "result" && m.meta?.context?.used);
  const lastTurn = turns[turns.length - 1];
  const last = lastTurn?.meta.context;
  if (!last) return null;
  const used = Number(last.used);
  // Claude Code reports the model's largest window, which can be 1M even
  // when the plan only gets 200k. Trust 1M only for a "[1m]" model or once
  // the chat is actually past 200k.
  const reported = Number(last.window) || 200_000;
  const long = /\[1m\]/i.test(String(last.model ?? "")) || used > 200_000;
  const size = reported > 200_000 && !long ? 200_000 : reported;
  // The session's very first model call: setup plus the first message.
  const base = Number(turns[0].meta.context.first_call ?? used);
  const tab = Math.min(Number(last.tab_context) || 0, base);
  const parts = [
    { label: "Agent setup", hint: "System prompt, tools and MCP servers", n: Math.max(0, base - tab), cls: "cw-setup" },
    { label: "tab context", hint: "This item and the context attached to it", n: tab, cls: "cw-tab" },
    { label: "Conversation", hint: "Your messages, replies and tool output", n: Math.max(0, used - base), cls: "cw-chat" },
  ];
  const pct = Math.min(100, (used / size) * 100);
  const tone = pct >= 90 ? "danger" : pct >= 75 ? "warning" : "accent";

  // Prompt cache: Claude keeps the conversation's prefix cached for about
  // five minutes after each call. A message sent while it's warm re-reads it
  // at a tenth of the price; after that the whole context is written again.
  const n = (key: string) => Number(last[key]) || 0;
  const read = n("cache_read"), write = n("cache_write"), fresh = n("fresh_input");
  const hit = read + write + fresh ? read / (read + write + fresh) : 0;
  const cached = read + write > 0;
  const left = cached ? Math.max(0, CACHE_TTL - (now - new Date(lastTurn.created_at).getTime())) : 0;
  const cache = !cached ? "none" : left <= 0 ? "cold" : left < 60_000 ? "cooling" : "warm";
  const total = turns.reduce((t, m) => {
    const c = m.meta.context;
    return { read: t.read + (Number(c.cache_read) || 0), all: t.all + (Number(c.cache_read) || 0) + (Number(c.cache_write) || 0) + (Number(c.fresh_input) || 0) };
  }, { read: 0, all: 0 });
  return { last, used, size, parts, pct, tone, read, write, fresh, hit, left, cache, total };
}

/** The context ring in the composer bar; click for the breakdown. */
export function ContextMeter({ messages }: { messages: SessionMessage[] }) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  const now = useNow(30_000);
  useEffect(() => {
    if (!open) return;
    const close = (e: MouseEvent) => ref.current && !ref.current.contains(e.target as Node) && setOpen(false);
    window.addEventListener("mousedown", close);
    return () => window.removeEventListener("mousedown", close);
  }, [open]);

  const u = contextUsage(messages, now);
  if (!u) return null;
  return (
    <div className="cw" ref={ref}>
      <button className="cw-chip" onClick={() => setOpen(!open)} title={`Context: ${k(u.used)} of ${k(u.size)} · cache ${cacheText(u.cache, u.left)}`}>
        <span className="cw-ring" style={{ ["--p" as string]: `${u.pct}%` }} data-tone={u.tone} />
        <span className="cw-used">{k(u.used)}</span>
        {u.cache !== "none" && <span className={cx("cw-cache", `cache-${u.cache}`)}>{u.cache === "cold" ? "cold" : clock(u.left)}</span>}
      </button>
      {open && <div className="cw-pop"><Breakdown u={u} /></div>}
    </div>
  );
}

/** The context breakdown on its own, for the `/context` card. */
export function ContextDetails({ messages }: { messages: SessionMessage[] }) {
  const now = useNow(30_000);
  const u = contextUsage(messages, now);
  if (!u) return <p className="subtle">Nothing yet: the breakdown shows once the agent has replied.</p>;
  return <Breakdown u={u} />;
}

function Breakdown({ u }: { u: Usage }) {
  const { last, used, size, parts, pct, tone, read, write, fresh, hit, left, cache, total } = u;
  return (
    <>
      <div className="cw-pop-head">
        <strong>Context window</strong>
        <span className={`text-${tone}`}>{k(used)} / {k(size)} · {Math.round(pct)}%</span>
      </div>
      <div className="cw-bar">
        {parts.map((p) => <span key={p.label} className={p.cls} style={{ width: `${(p.n / size) * 100}%` }} title={`${p.label}: ${k(p.n)}`} />)}
      </div>
      <ul className="cw-list">
        {parts.map((p) => (
          <li key={p.label}><i className={p.cls} /><span><strong>{p.label}</strong><small>{p.hint}</small></span><span>{k(p.n)}</span></li>
        ))}
        <li><i className="cw-free" /><span><strong>Free</strong></span><span>{k(Math.max(0, size - used))}</span></li>
      </ul>
      <div className="cw-pop-head">
        <strong>Prompt cache</strong>
        <span className={cx("cw-cache", `cache-${cache}`)}>{cacheText(cache, left)}</span>
      </div>
      <div className="cw-bar">
        <span className="cw-read" style={{ width: `${hit * 100}%` }} title={`Read from cache: ${k(read)}`} />
        <span className="cw-write" style={{ width: `${(read + write + fresh ? write / (read + write + fresh) : 0) * 100}%` }} title={`Written to cache: ${k(write)}`} />
        <span className="cw-fresh" style={{ width: `${(read + write + fresh ? fresh / (read + write + fresh) : 0) * 100}%` }} title={`Uncached: ${k(fresh)}`} />
      </div>
      <ul className="cw-list">
        <li><i className="cw-read" /><span><strong>Read from cache</strong><small>About a tenth of the input price</small></span><span>{k(read)}</span></li>
        <li><i className="cw-write" /><span><strong>Written to cache</strong><small>A little over the input price, once</small></span><span>{k(write)}</span></li>
        <li><i className="cw-fresh" /><span><strong>Uncached input</strong></span><span>{k(fresh)}</span></li>
      </ul>
      <p className="subtle">
        Last call {Math.round(hit * 100)}% cached{total.all ? `, ${Math.round((total.read / total.all) * 100)}% across this chat` : ""}.{" "}
        {cache === "cold"
          ? `The cache has expired, so the next message writes all ${k(used)} again.`
          : "Claude keeps it about 5 minutes after each reply. Changing model or mode starts a fresh cache."}
        {last.model ? ` · ${String(last.model).replace(/^claude-/, "")}` : ""}
        {pct >= 75 && " The agent compacts older history when it gets close to full."}
      </p>
    </>
  );
}

const duration = (ms: number) => {
  const s = Math.round(ms / 1000);
  return s < 60 ? `${s}s` : s < 3600 ? `${Math.floor(s / 60)}m ${s % 60}s` : `${Math.floor(s / 3600)}h ${Math.floor((s % 3600) / 60)}m`;
};

/**
 * What this chat has cost so far, for the `/cost` card: each turn's result
 * line carries its cost, time and model calls, and every turn runs the agent
 * afresh, so the chat's total is their sum.
 */
export function CostDetails({ messages }: { messages: SessionMessage[] }) {
  const turns = messages.filter((m) => m.kind === "result" && m.meta);
  if (!turns.length) return <p className="subtle">Nothing yet: costs show once the agent has replied.</p>;
  const sum = (f: (m: SessionMessage) => unknown) => turns.reduce((t, m) => t + (Number(f(m)) || 0), 0);
  const cost = sum((m) => m.meta.cost_usd);
  const priced = turns.some((m) => m.meta.cost_usd != null);
  const time = sum((m) => m.meta.duration_ms);
  const calls = sum((m) => m.meta.turns);
  const last = turns[turns.length - 1].meta;
  return (
    <>
      <ul className="cw-list">
        <li><i /><span><strong>Cost</strong><small>{priced ? "As the agent reports it; on a subscription this is what it would have cost" : "The agent didn't report a price"}</small></span><span>{priced ? `$${cost.toFixed(cost < 1 ? 3 : 2)}` : "—"}</span></li>
        <li><i /><span><strong>Agent time</strong><small>{turns.length} {turns.length === 1 ? "turn" : "turns"}, {calls} model {calls === 1 ? "call" : "calls"}</small></span><span>{duration(time)}</span></li>
        {last.cost_usd != null && <li><i /><span><strong>Last turn</strong></span><span>${Number(last.cost_usd).toFixed(3)} · {duration(Number(last.duration_ms) || 0)}</span></li>}
      </ul>
    </>
  );
}
