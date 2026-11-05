import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { Button, cx } from "../../design/components";
import { Brand } from "../../design/brands";
import { Icon } from "../../design/icons";
import { Markdown } from "../../design/markdown";
import { Thumbs } from "../../design/ImageViewer";
import { imageTypes, saveAttachment } from "../../lib/images";
import { Dictation } from "./Dictation";
import { api } from "../../lib/api";
import { useChat, useConfig, useStream } from "../../lib/hooks";
import { keys } from "../../lib/query";
import type { AgentKind, Approval, Chat, QueueItem, SessionMessage, SessionStatus } from "../../lib/types";

export const agentLabel: Record<AgentKind, string> = { claude_code: "Claude Code", opencode: "OpenCode" };
const agentBrand: Record<AgentKind, string> = { claude_code: "claude", opencode: "opencode" };

export function openOnboarding() {
  window.dispatchEvent(new CustomEvent("tab:onboarding"));
}

import { openItem } from "../../lib/links";
export { openItem, takePendingOpen } from "../../lib/links";

/**
 * Starts a chat whose agent sets up a connection, and opens it. The agent
 * finds settings itself (for email, the IMAP server from the address) and
 * asks for passwords only through a card in the chat.
 */
export async function startSetupChat(connectorName?: string) {
  const text = connectorName
    ? `Help me connect ${connectorName} to tab. Work out the settings for me and ask me only for what you can't find.`
    : "Help me connect a new service to tab. Ask me which one, then work out the settings for me.";
  const { item } = await api.chatSend(null, text);
  openItem(item.id);
}

function suggestions(item: QueueItem): string[] {
  const app = item.kind.split(".")[0];
  if (item.kind === "github.review_requested") return ["Review this PR and list the risks", "Check out the branch and run the tests", "Draft review comments"];
  if (item.kind === "github.ci_failed") return ["Find why CI failed and fix it", "Show me the failing test"];
  if (app === "slack" || app === "email") return ["Draft a reply", "Find everything related to this", "Summarize the thread"];
  if (item.kind === "meeting.action_item") return ["Start a draft of this", "Find related docs and tickets"];
  if (app === "tab") return ["Work on this", "Break this into steps", "Find related work in my queue"];
  return ["Summarize this and suggest next steps", "Work on this", "Find related work in my queue"];
}

/** The chat for one item. Its first message starts a session; later ones resume it. */
/**
 * The chat for one item. With `body`, that replaces the transcript (e.g. an
 * email being read) and `onSent` fires after a message goes out.
 */
export function ChatView({ item, body, showBody = true, onSent }: { item: QueueItem; body?: React.ReactNode; showBody?: boolean; onSent?: () => void }) {
  const { data: chat } = useChat(item.id);
  const qc = useQueryClient();
  const [error, setError] = useState<string | null>(null);

  const send = async (text: string, attachments: string[] = []) => {
    setError(null);
    onSent?.();
    // Show the message immediately; the stored copy replaces it.
    qc.setQueryData<Chat>(keys.chat(item.id), (c) => ({
      session: c?.session ?? null,
      approvals: c?.approvals ?? [],
      running: true,
      messages: [...(c?.messages ?? []), { id: -Date.now(), session_id: c?.session?.id ?? "", role: "user", kind: "text", content: text, meta: attachments.length ? { attachments } : null, created_at: new Date().toISOString() }],
    }));
    try {
      const started = await api.chatSend(item.id, text, null, attachments);
      qc.setQueryData<Chat>(keys.chat(item.id), (c) => ({
        session: started.session,
        approvals: c?.approvals ?? [],
        running: true,
        messages: (c?.messages ?? []).filter((m) => m.id > 0),
      }));
      qc.invalidateQueries({ queryKey: keys.chat(item.id) });
    } catch (e) {
      setError(String(e));
      qc.invalidateQueries({ queryKey: keys.chat(item.id) });
    }
  };

  return (
    <div className="chat">
      {/* The body stays mounted while hidden, so switching back doesn't reload it. */}
      {body && <div className="chat-body" hidden={!showBody}>{body}</div>}
      {(!body || !showBody) && <Transcript chat={chat} suggestions={suggestions(item)} onSuggest={send} onSend={send} />}
      {error && <div className="chat-error"><Icon name="x-circle" size={14} /> {error}</div>}
      <Composer chat={chat} onSend={send} placeholder={`Ask about this, or tell the agent what to do…`} />
    </div>
  );
}

const k = (n: number) => (n >= 10_000 ? `${Math.round(n / 1000)}k` : n >= 1000 ? `${(n / 1000).toFixed(1)}k` : `${n}`);

/**
 * How full the agent's context window is, from the last turn's usage, with
 * a breakdown: the agent's own setup (system prompt, tools, MCP), what tab
 * adds (this item and its attached context), and the conversation so far.
 */
function ContextMeter({ messages }: { messages: SessionMessage[] }) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  const now = useNow(30_000);
  useEffect(() => {
    if (!open) return;
    const close = (e: MouseEvent) => ref.current && !ref.current.contains(e.target as Node) && setOpen(false);
    window.addEventListener("mousedown", close);
    return () => window.removeEventListener("mousedown", close);
  }, [open]);

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
  const n = (k: string) => Number(last[k]) || 0;
  const read = n("cache_read"), write = n("cache_write"), fresh = n("fresh_input");
  const hit = read + write + fresh ? read / (read + write + fresh) : 0;
  const cached = read + write > 0;
  const left = cached ? Math.max(0, CACHE_TTL - (now - new Date(lastTurn.created_at).getTime())) : 0;
  const cache = !cached ? "none" : left <= 0 ? "cold" : left < 60_000 ? "cooling" : "warm";
  const total = turns.reduce((t, m) => {
    const c = m.meta.context;
    return { read: t.read + (Number(c.cache_read) || 0), all: t.all + (Number(c.cache_read) || 0) + (Number(c.cache_write) || 0) + (Number(c.fresh_input) || 0) };
  }, { read: 0, all: 0 });

  return (
    <div className="cw" ref={ref}>
      <button className="cw-chip" onClick={() => setOpen(!open)} title={`Context: ${k(used)} of ${k(size)} · cache ${cacheText(cache, left)}`}>
        <span className="cw-ring" style={{ ["--p" as string]: `${pct}%` }} data-tone={tone} />
        {k(used)}
        {cache !== "none" && <span className={cx("cw-cache", `cache-${cache}`)}>{cache === "cold" ? "cold" : clock(left)}</span>}
      </button>
      {open && (
        <div className="cw-pop">
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
        </div>
      )}
    </div>
  );
}

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

export function AgentStatus({ chat }: { chat: Chat | undefined }) {
  const s = chat?.session;
  if (!s) return null;
  const status: SessionStatus = chat.approvals.length ? "needs_input" : s.status;
  const text = { idle: "Idle", working: "Working…", needs_input: "Needs you", failed: "Failed" }[status];
  return (
    <span className={cx("agent-status", `agent-${status}`)}>
      <Brand name={agentBrand[s.agent]} size={12} />
      {agentLabel[s.agent]} · {text}
    </span>
  );
}

export function Transcript({ chat, suggestions, onSuggest, onSend, intro }: { chat: Chat | undefined; suggestions?: string[]; onSuggest?: (s: string) => void; onSend?: (text: string) => void; intro?: React.ReactNode }) {
  const stream = useStream(chat?.session?.id);
  const end = useRef<HTMLDivElement>(null);
  const messages = chat?.messages ?? [];
  const results = new Map<number, SessionMessage>();
  // Pair each tool call with the result that follows it.
  messages.forEach((m, i) => {
    if (m.kind === "tool" && messages[i + 1]?.kind === "tool_result") results.set(m.id, messages[i + 1]);
  });
  // Several password cards for one connection: show only the newest.
  const newestCard = new Map<string, number>();
  messages.forEach((m) => m.kind === "secret_request" && newestCard.set(m.meta?.connector_id, m.id));
  messages.forEach((m) => {
    if (m.kind === "secret_request" && newestCard.get(m.meta?.connector_id) !== m.id) m.meta = { ...m.meta, superseded: true };
  });

  useLayoutEffect(() => {
    end.current?.scrollIntoView({ block: "end" });
  }, [messages.length, stream, chat?.approvals.length]);

  if (!messages.length && !stream)
    return (
      <div className="transcript empty-chat">
        {intro}
        {suggestions && onSuggest && (
          <div className="suggestions">
            {suggestions.map((s) => (
              <button key={s} className="suggestion" onClick={() => onSuggest(s)}>
                <Icon name="zap" size={13} /> {s}
              </button>
            ))}
          </div>
        )}
      </div>
    );

  return (
    <div className="transcript">
      <div className="transcript-head"><AgentStatus chat={chat} /></div>
      {messages.map((m) => (m.kind === "tool_result" && [...results.values()].includes(m) ? null : <MessageView key={m.id} m={m} result={results.get(m.id)} onSend={onSend} />))}
      {stream && (
        <div className="msg msg-assistant streaming">
          <Markdown text={stream} />
        </div>
      )}
      {chat?.running && !stream && !chat.approvals.length && (
        <div className="msg-working"><span className="dots"><i /><i /><i /></span> Working</div>
      )}
      {chat?.approvals.map((a) => <ApprovalCard key={a.id} approval={a} />)}
      <div ref={end} />
    </div>
  );
}

function MessageView({ m, result, onSend }: { m: SessionMessage; result?: SessionMessage; onSend?: (text: string) => void }) {
  if (m.role === "user") {
    const images: string[] = Array.isArray(m.meta?.attachments) ? m.meta.attachments : [];
    return (
      <div className="msg msg-user">
        {images.length > 0 && <Thumbs srcs={images} size={96} />}
        {/```|\]\(/.test(m.content) ? <Markdown text={m.content} /> : m.content}
      </div>
    );
  }
  if (m.kind === "secret_request") return m.meta?.superseded ? null : <SecretCard m={m} onSend={onSend} />;
  if (m.kind === "text") return <div className="msg msg-assistant"><Markdown text={m.content} /></div>;
  if (m.kind === "tool" && m.meta?.tool === "AskUserQuestion") {
    // Answered through tab (or still waiting on its card): the card did the talking.
    if (!result || !result.meta?.is_error) return null;
    return <QuestionCard m={m} answered={false} onSend={onSend} />;
  }
  if (m.kind === "tool")
    return (
      <details className={cx("msg-tool", result?.meta?.is_error && "failed")}>
        <summary>
          <Icon name={m.meta?.tool?.startsWith("mcp__tab") || m.meta?.tool?.startsWith("tab_") ? "graph" : "terminal"} size={12} />
          <span>{m.content.replace(/^mcp__tab__/, "tab: ")}</span>
        </summary>
        {result?.content && <pre>{result.content}</pre>}
      </details>
    );
  if (m.kind === "tool_result") return <details className="msg-tool"><summary><Icon name="terminal" size={12} />Output</summary><pre>{m.content}</pre></details>;
  if (m.kind === "error") return <div className="msg-error"><Icon name="x-circle" size={14} /><span>{m.content}</span></div>;
  if (m.kind === "result") {
    const cost = m.meta?.cost_usd;
    const secs = m.meta?.duration_ms ? Math.round(m.meta.duration_ms / 1000) : null;
    if (cost == null && secs == null) return null;
    return <div className="msg-note">{[secs != null && `${secs}s`, cost != null && `$${Number(cost).toFixed(3)}`].filter(Boolean).join(" · ")}</div>;
  }
  return <div className="msg-note">{m.content}</div>;
}

interface Question { question: string; header?: string; multiSelect?: boolean; options?: { label: string; description?: string }[] }

/** The agent's questions with option buttons. Calls back with label(s) per question. */
function QuestionForm({ questions, done, onSubmit }: { questions: Question[]; done: boolean; onSubmit: (answers: Record<string, string>) => void }) {
  const [picked, setPicked] = useState<Record<number, string[]>>({});
  const [sent, setSent] = useState(done);
  const toggle = (qi: number, label: string, multi?: boolean) =>
    setPicked((p) => {
      const cur = p[qi] ?? [];
      return { ...p, [qi]: multi ? (cur.includes(label) ? cur.filter((x) => x !== label) : [...cur, label]) : [label] };
    });
  const submit = (all: Record<number, string[]>) => {
    const answers: Record<string, string> = {};
    questions.forEach((q, i) => all[i]?.length && (answers[q.question] = all[i].join(", ")));
    if (!Object.keys(answers).length) return;
    setSent(true);
    onSubmit(answers);
  };
  const single = questions.length === 1 && !questions[0].multiSelect;
  return (
    <div className={cx("question-card", sent && "sent")}>
      {questions.map((q, qi) => (
        <div key={qi} className="question">
          {q.header && <span className="label-sm">{q.header}</span>}
          <div className="question-text">{q.question}</div>
          <div className="question-options">
            {(q.options ?? []).map((o) => (
              <button
                key={o.label}
                className={cx("question-option", picked[qi]?.includes(o.label) && "active")}
                disabled={sent}
                onClick={() => (single ? submit({ [qi]: [o.label] }) : toggle(qi, o.label, q.multiSelect))}
              >
                <span>{o.label}</span>
                {o.description && <small>{o.description}</small>}
              </button>
            ))}
          </div>
        </div>
      ))}
      {!single && !sent && <Button size="sm" variant="primary" disabled={!Object.values(picked).some((v) => v.length)} onClick={() => submit(picked)}>Answer</Button>}
    </div>
  );
}

/**
 * AskUserQuestion that reached tab as a permission request: answering
 * returns the picks to the agent directly.
 */
function QuestionApproval({ approval }: { approval: Approval }) {
  const questions: Question[] = (approval.input as { questions?: Question[] })?.questions ?? [];
  return (
    <div>
      <QuestionForm questions={questions} done={false} onSubmit={(answers) => void api.approvalResolve(approval.id, "answer", undefined, answers)} />
      <button className="link-btn question-skip" onClick={() => void api.approvalResolve(approval.id, "deny", "The person skipped the question; ask in text instead.")}>Skip and answer in text</button>
    </div>
  );
}

/**
 * AskUserQuestion that the agent ran without asking tab (so it got no
 * answer): show the question anyway, and send a pick as the next message.
 */
function QuestionCard({ m, answered, onSend }: { m: SessionMessage; answered: boolean; onSend?: (text: string) => void }) {
  const questions: Question[] = m.meta?.input?.questions ?? [];
  if (!questions.length) return null;
  return (
    <QuestionForm
      questions={questions}
      done={answered || !onSend}
      onSubmit={(a) => onSend?.(Object.entries(a).map(([q, v]) => (questions.length > 1 ? `${q} ${v}` : v)).join("\n"))}
    />
  );
}

const MASK = "••••••••";

/**
 * Where the person types a connection's password or token. It goes straight
 * into tab's settings; the agent only hears that it was entered.
 */
function SecretCard({ m, onSend }: { m: SessionMessage; onSend?: (text: string) => void }) {
  const { data: config } = useConfig();
  const qc = useQueryClient();
  const fields: { key: string; label: string; help?: string | null }[] = m.meta?.fields ?? [];
  const id: string = m.meta?.connector_id;
  const [values, setValues] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState(false);
  const saved = config?.connectors[id]?.settings ?? {};
  const allSet = fields.every((f) => saved[f.key] === MASK);

  const submit = async () => {
    setBusy(true);
    setError(null);
    try {
      const next = await api.connectionSetSecrets(id, values);
      qc.setQueryData(keys.config, next);
      setDone(true);
      setValues({});
      onSend?.(`I entered my ${fields.map((f) => f.label.toLowerCase()).join(" and ")} for ${m.meta?.name ?? id}. Please test the connection.`);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <form className="approval secret-card" onSubmit={(e) => (e.preventDefault(), submit())}>
      <div className="approval-head"><Icon name="lock" size={14} /> {m.content}</div>
      {m.meta?.reason && <p className="subtle">{m.meta.reason}</p>}
      {fields.map((f) => (
        <label key={f.key} className="setting">
          {f.label}
          <span className="input">
            <input type="password" autoComplete="off" value={values[f.key] ?? ""} placeholder={allSet ? "Saved. Type to replace" : ""} onChange={(e) => setValues({ ...values, [f.key]: e.target.value })} />
          </span>
          {f.help && <small>{f.help}</small>}
        </label>
      ))}
      <p className="subtle">Saved straight to tab's settings on this computer. The agent never sees it.</p>
      {error && <div className="chat-error"><Icon name="x-circle" size={14} /> {error}</div>}
      <div className="toolbar">
        <Button type="submit" size="sm" variant="primary" icon="check" disabled={busy || fields.some((f) => !values[f.key]?.trim())}>
          {busy ? "Saving…" : "Save"}
        </Button>
        {(done || allSet) && <span className="subtle"><Icon name="check" size={12} /> Saved</span>}
      </div>
    </form>
  );
}

function ApprovalCard({ approval }: { approval: Approval }) {
  if (approval.tool === "AskUserQuestion") return <QuestionApproval approval={approval} />;
  return <ToolApproval approval={approval} />;
}

function ToolApproval({ approval }: { approval: Approval }) {
  const [busy, setBusy] = useState(false);
  const answer = async (d: "allow" | "always" | "deny") => {
    setBusy(true);
    await api.approvalResolve(approval.id, d);
  };
  return (
    <div className="approval">
      <div className="approval-head"><Icon name="shield" size={14} /> The agent wants to run</div>
      <code className="approval-what">{approval.summary}</code>
      <div className="toolbar">
        <Button size="sm" variant="primary" icon="check" disabled={busy} onClick={() => answer("allow")}>Allow</Button>
        <Button size="sm" disabled={busy} onClick={() => answer("always")}>Always allow</Button>
        <Button size="sm" variant="ghost" icon="x" disabled={busy} onClick={() => answer("deny")}>Deny</Button>
      </div>
    </div>
  );
}

const modelChoices: Record<AgentKind, { value: string; label: string }[]> = {
  claude_code: [
    { value: "claude-opus-5-5", label: "Opus 5.5" },
    { value: "claude-sonnet-5-5", label: "Sonnet 5.5" },
    { value: "claude-haiku-5-5", label: "Haiku 5.5" },
    { value: "claude-fable-5-1", label: "Fable 5.1" },
  ],
  opencode: [
    { value: "anthropic/claude-opus-5-5", label: "Opus 5.5" },
    { value: "anthropic/claude-sonnet-5-5", label: "Sonnet 5.5" },
    { value: "anthropic/claude-haiku-5-5", label: "Haiku 5.5" },
  ],
};

/** A compact picker for the composer bar: as wide as its label, menu above. */
function BarPicker({ value, options, onChange, title, className }: { value: string; options: { value: string; label: string; help?: string }[]; onChange: (v: string) => void; title: string; className?: string }) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLSpanElement>(null);
  useEffect(() => {
    if (!open) return;
    const close = (e: MouseEvent) => ref.current && !ref.current.contains(e.target as Node) && setOpen(false);
    const esc = (e: KeyboardEvent) => e.key === "Escape" && setOpen(false);
    window.addEventListener("mousedown", close);
    window.addEventListener("keydown", esc);
    return () => (window.removeEventListener("mousedown", close), window.removeEventListener("keydown", esc));
  }, [open]);
  const current = options.find((o) => o.value === value) ?? { value, label: value };
  return (
    <span className="bar-picker" ref={ref}>
      <button className={cx("bar-picker-btn", className)} onClick={() => setOpen((v) => !v)} title={current.help ? `${title}: ${current.help}` : title} aria-haspopup="menu" aria-expanded={open}>
        {current.label}
        <Icon name="chevron-down" size={11} />
      </button>
      {open && (
        <span className="bar-picker-menu" role="menu">
          {options.map((o) => (
            <button key={o.value} role="menuitemradio" aria-checked={o.value === value} className={cx(o.value === value && "on")} onClick={() => (setOpen(false), o.value !== value && onChange(o.value))}>
              <span>{o.label}</span>
              {o.help && <small>{o.help}</small>}
              {o.value === value && <Icon name="check" size={12} />}
            </button>
          ))}
        </span>
      )}
    </span>
  );
}

/** Which model chats run on. Applies from the next message, in every chat. */
function ModelPicker() {
  const { data: config } = useConfig();
  const qc = useQueryClient();
  const kind = config?.agent.kind;
  if (!kind) return null;
  const current = config.agent.model ?? "";
  const choices = modelChoices[kind];
  const options = [{ value: "", label: "Default model" }, ...choices];
  if (current && !choices.some((c) => c.value === current)) options.push({ value: current, label: current });
  return <BarPicker value={current} options={options} title="Model for chats, from the next message" onChange={async (v) => qc.setQueryData(keys.config, await api.setAgentModel(v || null))} />;
}

const modes = [
  { value: "default", label: "Ask first", help: "Asks before edits and commands" },
  { value: "acceptEdits", label: "Accept edits", help: "Edits files freely, asks before commands" },
  { value: "auto", label: "Auto", help: "Claude decides what's safe and asks about the rest" },
  { value: "plan", label: "Plan only", help: "Reads and plans, changes nothing" },
];

/** Claude Code's permission mode, saved at once. Applies from the next message. */
function ModePicker() {
  const { data: config } = useConfig();
  const qc = useQueryClient();
  if (config?.agent.kind !== "claude_code") return null;
  const current = config.agent.permission_mode || "default";
  const options = modes.some((m) => m.value === current) ? modes : [...modes, { value: current, label: current }];
  return <BarPicker value={current} options={options} title="Permission mode" className={cx(current === "auto" && "mode-auto")} onChange={async (v) => qc.setQueryData(keys.config, await api.setAgentMode(v))} />;
}

export function Composer({ chat, onSend, placeholder, autoFocus, extra }: { chat: Chat | undefined; onSend: (text: string, attachments: string[]) => void; placeholder: string; autoFocus?: boolean; extra?: React.ReactNode }) {
  const { data: config } = useConfig();
  const [text, setText] = useState("");
  const [images, setImages] = useState<string[]>([]);
  const [uploading, setUploading] = useState(0);
  const [dragging, setDragging] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const ref = useRef<HTMLTextAreaElement>(null);
  const file = useRef<HTMLInputElement>(null);
  const running = !!chat?.running;

  useEffect(() => {
    const t = ref.current;
    if (!t) return;
    t.style.height = "auto";
    t.style.height = `${Math.min(220, t.scrollHeight)}px`;
  }, [text]);

  useEffect(() => {
    if (autoFocus) ref.current?.focus();
  }, [autoFocus]);

  const add = async (files: File[]) => {
    const imgs = files.filter((f) => imageTypes.includes(f.type));
    if (files.length && !imgs.length) return setError("tab takes PNG, JPEG, GIF and WebP images.");
    setError(null);
    setUploading((n) => n + imgs.length);
    for (const f of imgs) {
      try {
        const path = await saveAttachment(f);
        setImages((xs) => [...xs, path]);
      } catch (e) {
        setError(String(e));
      } finally {
        setUploading((n) => n - 1);
      }
    }
  };

  const submit = () => {
    const t = text.trim();
    if ((!t && !images.length) || running || uploading) return;
    setText("");
    setImages([]);
    setError(null);
    onSend(t, images);
  };

  if (config && !config.agent.kind)
    return (
      <div className="composer composer-setup">
        <Icon name="bot" size={16} />
        <span>Chatting runs an agent on your machine. Set one up first.</span>
        <Button size="sm" variant="primary" onClick={openOnboarding}>Set up agent</Button>
      </div>
    );

  return (
    <div
      className={cx("composer", dragging && "dragging")}
      onDragOver={(e) => {
        if (![...e.dataTransfer.items].some((i) => i.kind === "file")) return;
        e.preventDefault();
        setDragging(true);
      }}
      onDragLeave={(e) => !e.currentTarget.contains(e.relatedTarget as Node) && setDragging(false)}
      onDrop={(e) => {
        if (!e.dataTransfer.files.length) return;
        e.preventDefault();
        setDragging(false);
        void add([...e.dataTransfer.files]);
      }}
    >
      {(images.length > 0 || uploading > 0) && (
        <div className="composer-images">
          <Thumbs srcs={images} size={56} onRemove={(i) => setImages((xs) => xs.filter((_, j) => j !== i))} />
          {uploading > 0 && <span className="thumb thumb-busy" style={{ width: 56, height: 56 }}><Icon name="refresh" size={14} /></span>}
        </div>
      )}
      <textarea
        ref={ref}
        rows={1}
        value={text}
        placeholder={placeholder}
        onChange={(e) => setText(e.target.value)}
        onPaste={(e) => {
          const files = [...e.clipboardData.files];
          if (!files.length) return;
          e.preventDefault();
          void add(files);
        }}
        onKeyDown={(e) => {
          if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) (e.preventDefault(), submit());
        }}
      />
      {error && <div className="composer-error">{error}</div>}
      <div className="composer-foot">
        <span className="subtle composer-agent">
          <button className="bar-icon" onClick={() => file.current?.click()} title="Attach images (or paste or drop them)" aria-label="Attach images"><Icon name="plus" size={14} /></button>
          <input ref={file} type="file" accept={imageTypes.join(",")} multiple hidden onChange={(e) => (void add([...(e.target.files ?? [])]), (e.target.value = ""))} />
          <Dictation onText={(t) => setText((cur) => (cur && !/\s$/.test(cur) ? `${cur} ${t}` : cur + t))} target={ref} />
          {config?.agent.kind && <><span className="composer-brand" title={agentLabel[config.agent.kind]}><Brand name={agentBrand[config.agent.kind]} size={11} /></span><ModelPicker /><ModePicker /></>}
          {extra}
          {chat?.messages && <ContextMeter messages={chat.messages} />}
        </span>
        {running && chat?.session ? (
          <Button size="sm" icon="stop" onClick={() => api.chatStop(chat.session!.id)}>Stop</Button>
        ) : (
          <Button size="sm" variant="primary" icon="arrow-up" disabled={(!text.trim() && !images.length) || uploading > 0} onClick={submit}>Send</Button>
        )}
      </div>
    </div>
  );
}
