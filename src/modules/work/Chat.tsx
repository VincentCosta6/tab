import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Button, cx } from "../../design/components";
import { Icon } from "../../design/icons";
import { Markdown } from "../../design/markdown";
import { Attachments } from "../../design/ImageViewer";
import { MarkdownFiles } from "../../design/markdown";
import { saveAttachment } from "../../lib/images";
import { AgentNotice } from "./AgentNotice";
import { Dictation } from "./Dictation";
import { useDropTarget } from "./DropZone";
import { api } from "../../lib/api";
import { useChat, useConfig, useHarnesses, useStream } from "../../lib/hooks";
import { HarnessIcon, ModelMenu, modelFor } from "./ModelMenu";
import { keys } from "../../lib/query";
import type { AgentKind, Approval, Chat, QueueItem, SessionMessage, SessionStatus } from "../../lib/types";


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

/**
 * What protects this chat: green "Sandboxed" where the OS enforces it, amber
 * "Limited sandbox" where it can't (Windows), nothing when it's off.
 */
export function SandboxPill() {
  const { data } = useQuery({ queryKey: ["sandbox"], queryFn: api.sandboxStatus, staleTime: 60_000 });
  if (!data?.enabled) return null;
  const github = "gh and git reach GitHub through tab with your GitHub connection; force pushes (except to a branch this chat created), merges and deletes ask you first.";
  return (
    <span className={cx("sandbox-pill", !data.enforced && "limited")} title={`${data.summary} ${github}`}>
      <Icon name="shield" size={11} /> {data.enforced ? "Sandboxed" : "Limited sandbox"}
    </span>
  );
}

/**
 * Strict mode: blue "Strict" when chats load only tab's connections, amber
 * "Not strict" when strict is on but this agent can't be held to it (an
 * OpenCode server you run yourself), nothing when it's off.
 */
export function StrictPill({ kind }: { kind?: AgentKind | null }) {
  const { data: config } = useConfig();
  const { data: harnesses } = useHarnesses();
  const agent = config?.agent;
  const harness = harnesses?.find((h) => h.id === (kind ?? agent?.kind));
  if (!agent || !harness || agent.strict === false) return null;
  // Its adapter can't keep your setup out, or it's attached to a server you run.
  const remote = !harness.capabilities.strict || (harness.capabilities.external_server && !!agent.opencode_url?.trim());
  const title = remote
    ? `Strict mode is on, but tab can't control what ${harness.name} loads here${harness.capabilities.external_server ? " (it's attached to a server you started)" : ""}.`
    : "Strict mode: this chat loads only tab's connections. Your personal MCP servers, connectors, plugins, hooks and user settings stay out; the repo's own config still loads. If anything else loads, tab stops the turn.";
  return (
    <span className={cx("strict-pill", remote && "limited")} title={title}>
      <Icon name="plug" size={11} /> {remote ? "Not strict" : "Strict"}
    </span>
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
  const { data: harnesses = [] } = useHarnesses();
  const s = chat?.session;
  if (!s) return null;
  const harness = harnesses.find((h) => h.id === s.agent);
  const status: SessionStatus = chat.approvals.length ? "needs_input" : s.status;
  const text = { idle: "Idle", working: "Working…", needs_input: "Needs you", monitoring: "Monitoring", failed: "Failed" }[status];
  return (
    <span className={cx("agent-status", `agent-${status}`)}>
      <HarnessIcon harness={harness} size={12} />
      {harness?.name ?? s.agent} · {text}
    </span>
  );
}

export function Transcript({ chat, suggestions, onSuggest, onSend, intro }: { chat: Chat | undefined; suggestions?: string[]; onSuggest?: (s: string) => void; onSend?: (text: string) => void; intro?: React.ReactNode }) {
  const stream = useStream(chat?.session?.id);
  const box = useRef<HTMLDivElement>(null);
  const stuck = useRef(true); // following the bottom; off once the person scrolls up to read
  const messages = chat?.messages ?? [];
  const results = new Map<number, SessionMessage>();
  // Pair each tool call with its result: by id when the agent ran several at once, else the one that follows it.
  const byUseId = new Map<string, SessionMessage>();
  messages.forEach((m) => m.kind === "tool_result" && m.meta?.tool_use_id && byUseId.set(m.meta.tool_use_id, m));
  messages.forEach((m, i) => {
    if (m.kind !== "tool") return;
    const r = (m.meta?.id && byUseId.get(m.meta.id)) || (messages[i + 1]?.kind === "tool_result" && !messages[i + 1].meta?.tool_use_id ? messages[i + 1] : undefined);
    if (r) results.set(m.id, r);
  });
  const paired = new Set(results.values());
  // Several password cards for one connection: show only the newest.
  const newestCard = new Map<string, number>();
  messages.forEach((m) => m.kind === "secret_request" && newestCard.set(m.meta?.connector_id, m.id));
  messages.forEach((m) => {
    if (m.kind === "secret_request" && newestCard.get(m.meta?.connector_id) !== m.id) m.meta = { ...m.meta, superseded: true };
  });

  // Follow new output while the person is at the bottom; leave them be once they scroll up.
  // Their own message always brings them back down.
  const last = messages[messages.length - 1];
  useLayoutEffect(() => {
    const el = box.current;
    if (!el) return;
    if (last?.role === "user") stuck.current = true;
    if (stuck.current) el.scrollTop = el.scrollHeight;
  }, [messages.length, stream, chat?.approvals.length, chat?.running, last?.role]);

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
    <MarkdownFiles.Provider value={{ base: chat?.session?.cwd ?? null }}>
    <div className="transcript" ref={box} onScroll={(e) => { const el = e.currentTarget; stuck.current = el.scrollHeight - el.scrollTop - el.clientHeight < 48; }}>
      <div className="transcript-head"><AgentStatus chat={chat} /></div>
      {groupTools(messages, paired).map((g) =>
        Array.isArray(g)
          ? <ToolGroup key={g[0].id} calls={g} results={results} />
          : <MessageView key={g.id} m={g} result={results.get(g.id)} onSend={onSend} />,
      )}
      {stream && (
        <div className="msg msg-assistant streaming">
          <Markdown text={stream} />
        </div>
      )}
      {chat?.approvals.map((a) => <ApprovalCard key={a.id} approval={a} />)}
      {chat?.running && chat.session && <SteerWait messages={messages} results={results} sessionId={chat.session.id} />}
      <ProgressBar messages={messages} results={results} running={!!chat?.running} />
    </div>
    </MarkdownFiles.Provider>
  );
}

/**
 * The agent reads a message sent mid-turn only between tool calls, as in
 * Claude Code's terminal. While a long tool holds it, offer "Send now"
 * (Esc there): stop that tool so the message is read right away.
 */
function SteerWait({ messages, results, sessionId }: { messages: SessionMessage[]; results: Map<number, SessionMessage>; sessionId: string }) {
  const [sent, setSent] = useState<number | null>(null);
  let steer = -1;
  for (let i = messages.length - 1; i >= 0; i--) {
    const m = messages[i];
    if (m.role === "assistant") break; // the agent has moved on since
    if (m.role === "user" && m.meta?.steer) { steer = i; break; }
  }
  if (steer < 0) return null;
  // The agent's latest step before the message: a tool call still without output.
  const tool = messages.slice(0, steer).reverse().find((m) => m.role === "assistant" && m.kind !== "suggestion");
  if (tool?.kind !== "tool" || tool.meta?.tool === "AskUserQuestion" || results.has(tool.id)) return null;
  const waiting = sent === messages[steer].id;
  return (
    <div className="msg-note steer-wait">
      {waiting ? "Stopping " : "Waiting for "}
      <span className="msg-tool-name">{toolName(tool.meta?.tool ?? "the tool")}</span>
      {waiting ? "…" : " to finish before reading your message"}
      {!waiting && (
        <Button size="sm" variant="ghost" onClick={() => (setSent(messages[steer].id), void api.chatInterrupt(sessionId))} title="Stop the running tool so your message is read now (Esc in Claude Code)">
          Send now
        </Button>
      )}
    </div>
  );
}

interface Todo { content: string; status: string; activeForm?: string }

const TODO_TOOLS = new Set(["TodoWrite", "todowrite"]);
const TASK_TOOLS = new Set(["TaskCreate", "TaskUpdate"]);
/** tab's own step tool (mcp__tab__tab_steps in Claude Code, tab_tab_steps in opencode). */
const isStepsTool = (tool?: string) => !!tool && /(^|_)tab_steps$/.test(tool);
/** Tools that only update the step list: shown in the step card, not as rows. */
const isStepTool = (tool?: string) => !!tool && (isStepsTool(tool) || TODO_TOOLS.has(tool) || TASK_TOOLS.has(tool) || tool === "TaskList" || tool === "TaskGet");

/** A file path's last part: `/a/b/Chat.tsx` reads as Chat.tsx. */
const base = (p: unknown) => (typeof p === "string" ? p.split("/").filter(Boolean).pop() ?? p : "");

/**
 * A tool call in plain words, for the step card when the agent gave no
 * plan: "Reading Chat.tsx", not the command line it ran.
 */
function describeTool(m: SessionMessage): string {
  const tool = toolName(m.meta?.tool ?? m.content);
  const input = m.meta?.input ?? {};
  const desc = typeof input.description === "string" ? input.description.trim() : "";
  switch (tool) {
    case "Bash":
    case "bash":
      return desc || "Running a command";
    case "Read":
    case "read":
      return `Reading ${base(input.file_path ?? input.filePath) || "a file"}`;
    case "Edit":
    case "edit":
    case "MultiEdit":
    case "NotebookEdit":
      return `Editing ${base(input.file_path ?? input.filePath ?? input.notebook_path) || "a file"}`;
    case "Write":
    case "write":
      return `Writing ${base(input.file_path ?? input.filePath) || "a file"}`;
    case "Grep":
    case "grep":
    case "Glob":
    case "glob":
      return "Searching the code";
    case "WebSearch":
    case "websearch":
      return "Searching the web";
    case "WebFetch":
    case "webfetch":
      return "Reading a web page";
    case "Agent":
    case "Task":
    case "task":
      return desc ? `Delegating: ${desc}` : "Delegating to a helper agent";
    case "ToolSearch":
      return "Loading tools";
  }
  if (tool.startsWith("tab_")) {
    const what: Record<string, string> = {
      tab_search: "Searching your queue",
      tab_get_item: "Reading a queue item",
      tab_attach_context: "Attaching context",
      tab_add_found: "Filing something under Found",
      tab_create_task: "Creating a follow-up task",
      tab_project: "Reading the project",
      tab_project_note: "Saving a project note",
      tab_graph_search: "Looking up what tab knows",
      tab_graph_expand: "Looking up what tab knows",
    };
    return what[tool] ?? "Working in tab";
  }
  return desc || tool.replace(/_/g, " ");
}

/**
 * The agent's step list, from whichever todo tool it uses: TodoWrite
 * (Claude Code, and opencode's todowrite) sends the whole list each time;
 * TaskCreate/TaskUpdate (newer Claude Code) builds it up one step at a time.
 * Only shown when this turn touched it, so an old list doesn't linger.
 */
function agentSteps(messages: SessionMessage[], results: Map<number, SessionMessage>): Todo[] {
  let turn = 0;
  for (let i = messages.length - 1; i >= 0; i--) if (messages[i].role === "user") { turn = i + 1; break; }
  for (let i = messages.length - 1; i >= turn; i--) {
    const m = messages[i];
    if (m.kind === "tool" && isStepsTool(m.meta?.tool)) {
      const steps = m.meta?.input?.steps;
      return Array.isArray(steps) ? steps.filter((t) => t && typeof t.title === "string").map((t) => ({ content: t.title, status: String(t.status ?? "pending") })) : [];
    }
    if (m.kind === "tool" && TODO_TOOLS.has(m.meta?.tool)) {
      const todos = m.meta?.input?.todos;
      return Array.isArray(todos) ? todos.filter((t) => t && typeof t.content === "string") : [];
    }
  }
  if (!messages.slice(turn).some((m) => m.kind === "tool" && TASK_TOOLS.has(m.meta?.tool))) return [];
  const tasks = new Map<string, Todo>();
  let next = 1;
  for (const m of messages) {
    if (m.kind !== "tool") continue;
    const input = m.meta?.input ?? {};
    if (m.meta?.tool === "TaskCreate") {
      const id = /#(\d+)/.exec(results.get(m.id)?.content ?? "")?.[1] ?? String(next);
      next = Math.max(next, Number(id) || 0) + 1;
      tasks.set(id, { content: String(input.subject ?? input.description ?? "Step"), activeForm: input.activeForm, status: "pending" });
    } else if (m.meta?.tool === "TaskUpdate") {
      const id = String(input.taskId ?? "");
      const t = tasks.get(id);
      if (!t) continue;
      if (input.status === "deleted") tasks.delete(id);
      else tasks.set(id, { ...t, status: input.status ?? t.status, content: input.subject ?? t.content, activeForm: input.activeForm ?? t.activeForm });
    }
  }
  return [...tasks.values()];
}

/**
 * The last thing in the chat, never over it: the agent's plan for this turn
 * with each step's status, and the tool it's running right now.
 */
function ProgressBar({ messages, results, running }: { messages: SessionMessage[]; results: Map<number, SessionMessage>; running: boolean }) {
  // Open while the agent works, folded to one line once it's done (click to open either way).
  const [toggled, setToggled] = useState<boolean | null>(null);
  useEffect(() => setToggled(null), [running]);
  const collapsed = toggled ?? !running;
  const setCollapsed = setToggled;
  const plan = agentSteps(messages, results);
  // No plan from the agent: show what it's doing this turn as steps instead,
  // the last few tool calls with done/doing marks, so there's always something to follow.
  const activity: Todo[] = [];
  if (!plan.length && running) {
    let turn = 0;
    for (let i = messages.length - 1; i >= 0; i--) if (messages[i].role === "user") { turn = i + 1; break; }
    for (const m of messages.slice(turn)) {
      if (m.kind !== "tool" || isStepTool(m.meta?.tool) || m.meta?.tool === "AskUserQuestion") continue;
      activity.push({ content: describeTool(m), status: results.has(m.id) ? "completed" : "in_progress" });
    }
  }
  const earlier = Math.max(0, activity.length - 5);
  const todos = plan.length ? plan : activity.slice(-5);
  let step = "";
  for (let i = messages.length - 1; i >= 0; i--) {
    const m = messages[i];
    if (m.role === "user") break; // only this turn's work
    if (m.kind !== "tool") continue;
    const tool = m.meta?.tool;
    if (isStepTool(tool) || tool === "AskUserQuestion") continue;
    step = m.content.replace(/^mcp__tab__/, "tab: ").split("\n")[0];
    break;
  }
  if (!running && !todos.length) return null;
  const done = todos.filter((t) => t.status === "completed").length;
  const active = plan.find((t) => t.status === "in_progress");
  const label = active ? active.activeForm || active.content : step || "Working";
  const count = plan.length ? `${done} of ${plan.length}` : activity.length ? `${activity.length} tool call${activity.length > 1 ? "s" : ""}` : "";
  return (
    <div className={cx("progress-bar", running && "running")}>
      <button className="progress-head" onClick={() => todos.length && setCollapsed(!collapsed)} aria-expanded={!collapsed}>
        {running ? <span className="dots"><i /><i /><i /></span> : <Icon name="check" size={12} />}
        <span className="progress-label">{running ? (collapsed || !todos.length ? label : "Working") : "Done"}</span>
        {count && <span className="progress-count">{count}</span>}
      </button>
      {todos.length > 0 && !collapsed && (
        <ul className={cx("progress-list", !plan.length && "activity")}>
          {earlier > 0 && <li className="progress-earlier">{earlier} earlier</li>}
          {todos.map((t, i) => (
            <li key={i} className={t.status}>
              <span className={cx("step-mark", t.status, !running && "still")}>{t.status === "completed" && <Icon name="check" size={9} />}</span>
              <span>{t.status === "in_progress" && running ? t.activeForm || t.content : t.content}</span>
            </li>
          ))}
        </ul>
      )}
      {running && step && active && <div className="progress-step">{step}</div>}
    </div>
  );
}

/** Tool calls the person sees as rows (not step updates or questions, which have their own cards). */
const isRow = (m: SessionMessage) => m.kind === "tool" && !isStepTool(m.meta?.tool) && m.meta?.tool !== "AskUserQuestion";

/**
 * The transcript with runs of back-to-back tool calls gathered into one
 * group each, so a long stretch of work reads as one line, not fifty.
 * Step updates and paired results inside a run don't break it.
 */
function groupTools(messages: SessionMessage[], paired: Set<SessionMessage>): (SessionMessage | SessionMessage[])[] {
  const out: (SessionMessage | SessionMessage[])[] = [];
  let run: SessionMessage[] | null = null;
  for (const m of messages) {
    if (m.kind === "tool_result" && paired.has(m)) continue;
    if (m.kind === "tool" && isStepTool(m.meta?.tool)) continue;
    if (isRow(m)) {
      if (!run) out.push((run = []));
      run.push(m);
      continue;
    }
    run = null;
    out.push(m);
  }
  return out;
}

/** A run of tool calls: one line saying how many and which, opening to each call. */
function ToolGroup({ calls, results }: { calls: SessionMessage[]; results: Map<number, SessionMessage> }) {
  if (calls.length === 1) return <ToolRow m={calls[0]} result={results.get(calls[0].id)} />;
  const names = [...new Set(calls.map((c) => toolName(c.meta?.tool ?? c.content)))];
  const failed = calls.filter((c) => results.get(c.id)?.meta?.is_error).length;
  return (
    <details className={cx("msg-tool msg-tool-group", failed > 0 && "failed")}>
      <summary>
        <Icon name="terminal" size={12} />
        <span className="msg-tool-name">
          {calls.length} tool calls · {names.slice(0, 4).join(", ")}{names.length > 4 ? ` +${names.length - 4}` : ""}
        </span>
        {failed > 0 && <span className="msg-tool-flag">{failed} failed</span>}
      </summary>
      <div className="msg-tool-list">
        {calls.map((c) => <ToolRow key={c.id} m={c} result={results.get(c.id)} />)}
      </div>
    </details>
  );
}

/** A tool's short name: `mcp__tab__tab_search` reads as tab_search. */
const toolName = (t: string) => t.replace(/^mcp__[^_]+(?:_[^_]+)*?__/, "").split(":")[0];

/**
 * One tool call: a single line (first line of the command, cut to fit),
 * opening to the full command and its output, each scrolling on its own.
 */
function ToolRow({ m, result }: { m: SessionMessage; result?: SessionMessage }) {
  const tool: string = m.meta?.tool ?? "";
  const label = m.content.replace(/^mcp__tab__/, "tab: ");
  const [first, ...more] = label.split("\n");
  const command: string | undefined = typeof m.meta?.input?.command === "string" ? m.meta.input.command : undefined;
  const long = !!command && (command.includes("\n") || command.length > 120);
  return (
    <details className={cx("msg-tool", result?.meta?.is_error && "failed")}>
      <summary title={label.length > 300 ? label.slice(0, 300) + "…" : label}>
        <Icon name={tool.startsWith("mcp__tab") || tool.startsWith("tab_") ? "graph" : "terminal"} size={12} />
        <span className="msg-tool-name">{first}{more.length > 0 && " …"}</span>
        {result?.meta?.is_error && <span className="msg-tool-flag">failed</span>}
      </summary>
      {long && <pre className="msg-tool-cmd">{command}</pre>}
      {result?.content ? <pre>{result.content}</pre> : !long && <div className="msg-tool-empty">No output</div>}
    </details>
  );
}

function MessageView({ m, result, onSend }: { m: SessionMessage; result?: SessionMessage; onSend?: (text: string) => void }) {
  if (m.kind === "suggestion") return null;
  if (m.role === "user") {
    const images: string[] = Array.isArray(m.meta?.attachments) ? m.meta.attachments : [];
    return (
      <div className="msg msg-user">
        {images.length > 0 && <Attachments paths={images} size={96} />}
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
  // Step-list updates show in the pinned step card, not as rows.
  if (m.kind === "tool" && isStepTool(m.meta?.tool)) return null;
  if (m.kind === "tool")
    return (
      <ToolRow m={m} result={result} />
    );
  if (m.kind === "tool_result") return <details className="msg-tool"><summary><Icon name="terminal" size={12} /><span className="msg-tool-name">Output</span></summary><pre>{m.content}</pre></details>;
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
  // Free-text "Other" answer per question; it counts as a pick of its own.
  const [other, setOther] = useState<Record<number, string>>({});
  const toggle = (qi: number, label: string, multi?: boolean) =>
    setPicked((p) => {
      const cur = p[qi] ?? [];
      return { ...p, [qi]: multi ? (cur.includes(label) ? cur.filter((x) => x !== label) : [...cur, label]) : [label] };
    });
  const submit = (all: Record<number, string[]>) => {
    const answers: Record<string, string> = {};
    questions.forEach((q, i) => {
      const typed = other[i]?.trim();
      // Single-select: typed text replaces the button pick. Multi-select: it's added.
      const picks = q.multiSelect ? [...(all[i] ?? []), ...(typed ? [typed] : [])] : typed && !all[i]?.length ? [typed] : all[i] ?? [];
      if (picks.length) answers[q.question] = picks.join(", ");
    });
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
            <input
              className="question-other"
              disabled={sent}
              placeholder="Other: type your own answer"
              value={other[qi] ?? ""}
              onChange={(e) => setOther({ ...other, [qi]: e.target.value })}
              onKeyDown={(e) => {
                if (e.key !== "Enter" || !other[qi]?.trim()) return;
                e.preventDefault();
                single ? submit({}) : submit(picked);
              }}
            />
          </div>
        </div>
      ))}
      {!sent && (!single || Object.values(other).some((v) => v.trim())) && (
        <Button size="sm" variant="primary" disabled={!Object.values(picked).some((v) => v.length) && !Object.values(other).some((v) => v.trim())} onClick={() => submit(picked)}>Answer</Button>
      )}
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

/**
 * Which model this chat runs on, from every installed harness. Before the
 * chat starts any harness can be picked (and becomes the one new chats use);
 * after, it stays on its harness, and the pick applies from the next message
 * to that harness's chats.
 */
function ChatModelPicker({ chat }: { chat: Chat | undefined }) {
  const { data: config } = useConfig();
  const qc = useQueryClient();
  const agent = chat?.session?.agent ?? config?.agent.kind;
  if (!config || !agent) return null;
  const started = !!chat?.session;
  return (
    <ModelMenu
      value={{ agent, model: modelFor(config.agent, agent) }}
      lock={started ? agent : undefined}
      title="Model for this chat, from the next message"
      onChange={async (p) => qc.setQueryData(keys.config, await api.setAgentModel(p.model, p.agent, !started))}
    />
  );
}

/** The harness's permission mode (from its metadata), saved at once. Applies from the next message. */
function ModePicker({ chat }: { chat: Chat | undefined }) {
  const { data: config } = useConfig();
  const { data: harnesses } = useHarnesses();
  const qc = useQueryClient();
  const agent = chat?.session?.agent ?? config?.agent.kind;
  const modes = harnesses?.find((h) => h.id === agent)?.capabilities.permission_modes.map((m) => ({ ...m, help: m.help ?? undefined })) ?? [];
  if (!config || !modes.length) return null;
  const current = config.agent.permission_mode || "default";
  const options = modes.some((m) => m.value === current) ? modes : [...modes, { value: current, label: current }];
  return <BarPicker value={current} options={options} title="Permission mode" className={cx(current === "auto" && "mode-auto")} onChange={async (v) => qc.setQueryData(keys.config, await api.setAgentMode(v))} />;
}

/** What Claude Code's built-in commands do; custom commands and skills show just their name. */
const slashDescriptions: Record<string, string> = {
  compact: "Summarise the conversation so far to free up context",
  context: "Show what's using the context window",
  cost: "Show this session's cost and duration",
  init: "Write a CLAUDE.md for this codebase",
  mcp: "Summarise the MCP servers this agent has",
  status: "Show the agent's version, model and account",
  memory: "Show the memory files loaded",
  agents: "List the subagents available",
  hooks: "List the hooks configured",
  model: "Show the model in use",
  review: "Review a pull request",
  "pr-comments": "Fetch the comments on a pull request",
  "security-review": "Review the pending changes for security issues",
  "release-notes": "Show Claude Code's release notes",
  todos: "List the current todo items",
};
// Terminal-only commands that do nothing useful in a headless turn.
const interactiveOnly = new Set(["config", "permissions", "login", "logout", "vim", "exit", "quit", "theme", "terminal-setup", "ide", "resume", "statusline", "upgrade", "bug", "feedback", "privacy-settings", "output-style", "install-github-app", "migrate-installer", "rewind", "add-dir", "export", "clear"]);
const fallbackSlash = ["compact", "context", "cost", "mcp", "status", "init", "review", "pr-comments", "security-review"];

/**
 * The `/` menu: Claude Code's commands, custom commands and skills, as it
 * reported them when it last started. The command is sent as typed and
 * Claude Code runs it.
 */
function useSlashMenu(text: string) {
  const { data } = useQuery({ queryKey: ["slash-commands"], queryFn: api.slashCommands, staleTime: 60_000 });
  const [index, setIndex] = useState(0);
  const [dismissed, setDismissed] = useState<string | null>(null);
  const m = /^\/([\w:.-]*)$/.exec(text);
  const q = m?.[1].toLowerCase() ?? "";
  const names = data?.length ? data : fallbackSlash;
  const matches = m
    ? names
        .filter((n) => !interactiveOnly.has(n) && n.toLowerCase().includes(q))
        .sort((a, b) => Number(!a.toLowerCase().startsWith(q)) - Number(!b.toLowerCase().startsWith(q)))
        .slice(0, 8)
        .map((name) => ({ name, description: slashDescriptions[name] }))
    : [];
  const open = matches.length > 0 && dismissed !== text;
  useEffect(() => setIndex(0), [q]);
  const handleKey = (e: React.KeyboardEvent, pick: (name: string) => void) => {
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      const d = e.key === "ArrowDown" ? 1 : -1;
      setIndex((i) => (i + d + matches.length) % matches.length);
      return true;
    }
    if (e.key === "Tab" || (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing)) {
      const c = matches[Math.min(index, matches.length - 1)];
      // A full command already typed sends on Enter.
      if (e.key === "Enter" && c.name.toLowerCase() === q) return false;
      e.preventDefault();
      pick(c.name);
      return true;
    }
    if (e.key === "Escape") {
      e.preventDefault();
      setDismissed(text);
      return true;
    }
    return false;
  };
  return { open, matches, index, setIndex, handleKey, close: () => setDismissed(null) };
}

export function Composer({ chat, onSend, placeholder, autoFocus, extra }: { chat: Chat | undefined; onSend: (text: string, attachments: string[]) => void; placeholder: string; autoFocus?: boolean; extra?: React.ReactNode }) {
  const { data: config } = useConfig();
  const [text, setText] = useState("");
  const [images, setImages] = useState<string[]>([]);
  const [uploading, setUploading] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const ref = useRef<HTMLTextAreaElement>(null);
  const file = useRef<HTMLInputElement>(null);
  const running = !!chat?.running;
  const last = chat?.messages[chat.messages.length - 1];
  const suggestion = !running && last?.kind === "suggestion" && config?.agent.suggest_replies !== false ? last.content : "";
  const slash = useSlashMenu(text);
  const pickSlash = (name: string) => (setText(`/${name} `), slash.close(), ref.current?.focus());

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
    const big = files.filter((f) => f.size > 50 * 1024 * 1024);
    const ok = files.filter((f) => f.size <= 50 * 1024 * 1024);
    setError(big.length ? `${big.map((f) => f.name).join(", ")} ${big.length > 1 ? "are" : "is"} over 50 MB.` : null);
    setUploading((n) => n + ok.length);
    for (const f of ok) {
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

  const drop = useDropTarget((files) => void add(files));

  const submit = () => {
    const t = text.trim();
    if ((!t && !images.length) || uploading) return;
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
    <div ref={drop.ref} className={cx("composer", drop.armed && "drop-armed", drop.targeted && "dragging")}>
      <AgentNotice messages={chat?.messages} />
      {(images.length > 0 || uploading > 0) && (
        <div className="composer-images">
          <Attachments paths={images} size={56} onRemove={(p) => setImages((xs) => xs.filter((x) => x !== p))} />
          {uploading > 0 && <span className="thumb thumb-busy" style={{ width: 56, height: 56 }}><Icon name="refresh" size={14} /></span>}
        </div>
      )}
      <textarea
        ref={ref}
        rows={1}
        value={text}
        placeholder={suggestion || (running ? "Steer the agent while it works…" : placeholder)}
        onChange={(e) => setText(e.target.value)}
        onPaste={(e) => {
          const files = [...e.clipboardData.files];
          if (!files.length) return;
          e.preventDefault();
          void add(files);
        }}
        onKeyDown={(e) => {
          if (slash.open && slash.handleKey(e, pickSlash)) return;
          // Tab takes the suggested reply; Enter then sends it.
          if (e.key === "Tab" && !e.shiftKey && !text && suggestion) return (e.preventDefault(), setText(suggestion));
          if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) (e.preventDefault(), submit());
        }}
      />
      {slash.open && (
        <div className="slash-menu" role="listbox">
          {slash.matches.map((c, i) => (
            <button key={c.name} role="option" aria-selected={i === slash.index} className={cx(i === slash.index && "active")} onMouseDown={(e) => (e.preventDefault(), pickSlash(c.name))} onMouseEnter={() => slash.setIndex(i)}>
              <span className="slash-name">/{c.name}</span>
              {c.description && <span className="slash-desc">{c.description}</span>}
            </button>
          ))}
        </div>
      )}
      {error && <div className="composer-error">{error}</div>}
      {suggestion && !text && <div className="composer-suggest subtle"><kbd>Tab</kbd> to use the suggested reply, then <kbd>Enter</kbd> to send</div>}
      <div className="composer-foot">
        <span className="subtle composer-agent">
          <button className="bar-icon" onClick={() => file.current?.click()} title="Attach files or images (or paste or drop them)" aria-label="Attach files"><Icon name="plus" size={14} /></button>
          <input ref={file} type="file" multiple hidden onChange={(e) => (void add([...(e.target.files ?? [])]), (e.target.value = ""))} />
          <Dictation onText={(t) => setText((cur) => (cur && !/\s$/.test(cur) ? `${cur} ${t}` : cur + t))} target={ref} />
          {config?.agent.kind && <><ChatModelPicker chat={chat} /><ModePicker chat={chat} /></>}
          {extra}
          {chat?.messages && <ContextMeter messages={chat.messages} />}
        </span>
        {running && chat?.session && (
          <Button size="sm" icon="stop" onClick={() => api.chatStop(chat.session!.id)}>Stop</Button>
        )}
        <Button size="sm" variant="primary" icon="arrow-up" disabled={(!text.trim() && !images.length) || uploading > 0} onClick={submit}>{running ? "Steer" : "Send"}</Button>
      </div>
    </div>
  );
}
