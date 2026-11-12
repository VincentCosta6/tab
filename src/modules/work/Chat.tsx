import { Fragment, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Button, cx } from "../../design/components";
import { Icon } from "../../design/icons";
import { Attachments } from "../../design/ImageViewer";
import { FilePreview } from "../../design/FileStage";
import { ImageThumb, Markdown, MarkdownFiles } from "../../design/markdown";
import { fileName, isImagePath, saveAttachment } from "../../lib/images";
import { filesInText, isShowable } from "../../lib/threadFiles";
import { useStage } from "../../lib/stage";
import { sessionLook } from "../shell/AppHeader";
import { AgentNotice } from "./AgentNotice";
import { ContextMeter } from "./ContextMeter";
import { CompactedDivider } from "../projects/Compaction";
import { SlashPanel, tabCommand, tabCommands, type Panel } from "./SlashPanels";
import { openSlots, parseHint, splitArgs, type ArgSource } from "./slashArgs";
import { Dictation } from "./Dictation";
import { useDropTarget } from "./DropZone";
import { api } from "../../lib/api";
import { useChat, useConfig, useHarnesses, useModels, useStream } from "../../lib/hooks";
import { HarnessIcon, ModelMenu, modelFor } from "./ModelMenu";
import { keys } from "../../lib/query";
import type { AgentKind, Approval, Chat, QueueItem, Session, SessionMessage, SessionStatus, SlashCommand, SlashKind } from "../../lib/types";


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

  const shell = async (command: string) => {
    setError(null);
    try {
      const started = await api.chatShell(item.id, command);
      qc.setQueryData<Chat>(keys.chat(item.id), (c) => ({ session: started.session, approvals: c?.approvals ?? [], running: c?.running ?? false, messages: c?.messages ?? [] }));
      qc.invalidateQueries({ queryKey: keys.chat(item.id) });
    } catch (e) {
      setError(String(e));
    }
  };

  return (
    <div className="chat">
      {/* The body stays mounted while hidden, so switching back doesn't reload it. */}
      {body && <div className="chat-body" hidden={!showBody}>{body}</div>}
      {(!body || !showBody) && <Transcript chat={chat} suggestions={suggestions(item)} onSuggest={send} onSend={send} />}
      {error && <div className="chat-error"><Icon name="x-circle" size={14} /> {error}</div>}
      <Composer chat={chat} onSend={send} onShell={shell} placeholder={`Ask about this, or tell the agent what to do…`} />
    </div>
  );
}

/**
 * Where this chat runs, as small as it gets: a computer or cloud, the
 * device's name, and a dot for whether it's connected (the rest on hover).
 */
export function DevicePill({ session }: { session: Session }) {
  const hub = useQuery({ queryKey: ["hub-status"], queryFn: api.hubStatus, refetchInterval: 5000 });
  const runners = useQuery({ queryKey: ["runners"], queryFn: api.runners, refetchInterval: 30_000 });
  const list = runners.data ?? [];
  const l = sessionLook(session, list, hub.data ?? null);
  const name = l.place === "This computer" ? list.find((r) => r.this_app)?.name || "This computer" : l.place;
  return (
    <span className="device-pill" title={`${l.label}. ${l.detail}`}>
      <Icon name={l.icon} size={11} />
      <span className="device-pill-name">{name}</span>
      <span className={cx("header-chip-state", `tone-${l.tone}`)}><i className="header-dot" aria-hidden="true" /></span>
    </span>
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
  const remote = !harness.capabilities.strict || (harness.capabilities.external_server && !!agent.opencode_url?.trim() && !agent.acp?.includes(harness.id));
  const title = remote
    ? `Strict mode is on, but tab can't control what ${harness.name} loads here${harness.capabilities.external_server ? " (it's attached to a server you started)" : ""}.`
    : "Strict mode: this chat loads only tab's connections. Your personal MCP servers, connectors, plugins, hooks and user settings stay out; the repo's own config still loads. If anything else loads, tab stops the turn.";
  return (
    <span className={cx("strict-pill", remote && "limited")} title={title}>
      <Icon name="plug" size={11} /> {remote ? "Not strict" : "Strict"}
    </span>
  );
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

/** Chat turns drawn at a time. */
const TURN_PAGE = 30;

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

  const base = chat?.session?.cwd ?? null;
  // Claude Code keeps a checkpoint per message; going back needs the turn over.
  const rewindable = chat?.session?.agent === "claude_code" && !chat.running;
  const folded = useMemo(() => foldedTexts(messages, !!chat?.running), [messages, chat?.running]);
  const turns = useMemo(() => splitTurns(messages, base), [messages, base]);

  // Draw the latest turns only; earlier ones come in as the person scrolls up.
  // Every message costs DOM, and a long chat held hundreds of MB.
  const [drawnTurns, setDrawnTurns] = useState(TURN_PAGE);
  const hidden = Math.max(0, turns.length - drawnTurns);
  const heightBefore = useRef<number | null>(null);
  const showEarlier = () => {
    if (heightBefore.current !== null) return;
    heightBefore.current = box.current?.scrollHeight ?? 0;
    setDrawnTurns((n) => n + TURN_PAGE);
  };
  // Keep what the person was reading in place as turns are added above it.
  useLayoutEffect(() => {
    const el = box.current;
    if (el && heightBefore.current !== null) el.scrollTop += el.scrollHeight - heightBefore.current;
    heightBefore.current = null;
  }, [drawnTurns]);

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
    <MarkdownFiles.Provider value={{ base: chat?.session?.cwd ?? null, imagesAtEnd: true }}>
    <div className="transcript" ref={box} onScroll={(e) => {
      const el = e.currentTarget;
      stuck.current = el.scrollHeight - el.scrollTop - el.clientHeight < 48;
      if (hidden && el.scrollTop < 400) showEarlier();
    }}>
      <div className="transcript-head"><AgentStatus chat={chat} /></div>
      {hidden > 0 && (
        <div className="transcript-earlier">
          <Button variant="ghost" size="sm" onClick={showEarlier}>Show {Math.min(hidden, TURN_PAGE)} earlier turn{Math.min(hidden, TURN_PAGE) === 1 ? "" : "s"} of {hidden}</Button>
        </div>
      )}
      {turns.slice(hidden).map((t) => (
        <Fragment key={t.messages[0].id}>
          {groupTools(t.messages, paired, folded).map((g) =>
            Array.isArray(g)
              ? <ToolGroup key={g[0].id} calls={g} results={results} />
              : <MessageView key={g.id} m={g} result={results.get(g.id)} onSend={onSend} rewind={rewindable && chat?.session ? chat.session.id : undefined} />,
          )}
          {t.files.length > 0 && <TurnFiles paths={t.files} />}
        </Fragment>
      ))}
      {chat?.approvals.map((a) => <ApprovalCard key={a.id} approval={a} />)}
      {chat?.running && chat.session && <SteerWait messages={messages} results={results} sessionId={chat.session.id} />}
      <ProgressBar messages={messages} results={results} running={!!chat?.running} folded={folded} writing={!!stream} />
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
 * Where the running (or last) turn starts: just after the person's last
 * message that started one. A message sent mid-turn (a steer) joins the
 * turn already running, so the plan from before it still counts.
 */
function turnStart(messages: SessionMessage[]): number {
  for (let i = messages.length - 1; i >= 0; i--) if (messages[i].role === "user" && !messages[i].meta?.steer) return i + 1;
  return 0;
}

/**
 * The agent's step list, from whichever todo tool it uses: TodoWrite
 * (Claude Code, and opencode's todowrite) sends the whole list each time;
 * TaskCreate/TaskUpdate (newer Claude Code) builds it up one step at a time.
 * Only shown when this turn touched it, so an old list doesn't linger.
 */
function agentSteps(messages: SessionMessage[], results: Map<number, SessionMessage>): Todo[] {
  const turn = turnStart(messages);
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
 * with each step's status, and what it's doing right now. A step that's
 * under way spins, so "Working" shows only when none is (at the foot of the
 * list, where the work is), never as a second spinner beside a step's.
 */
function ProgressBar({ messages, results, running, folded, writing }: { messages: SessionMessage[]; results: Map<number, SessionMessage>; running: boolean; folded: Set<number>; writing: boolean }) {
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
    const turn = turnStart(messages);
    for (const m of messages.slice(turn)) {
      if (m.kind !== "tool" || isStepTool(m.meta?.tool) || m.meta?.tool === "AskUserQuestion") continue;
      activity.push({ content: describeTool(m), status: results.has(m.id) ? "completed" : "in_progress" });
    }
  }
  const earlier = Math.max(0, activity.length - 5);
  const todos = plan.length ? plan : activity.slice(-5);
  // What it's doing now, in its own words if it said (its latest working-out), else the tool it ran.
  let step = "";
  let note = "";
  for (let i = messages.length - 1; i >= 0; i--) {
    const m = messages[i];
    if (m.role === "user" && !m.meta?.steer) break; // only this turn's work
    if (!note && folded.has(m.id)) note = m.content.trim().split("\n")[0].replace(/[`*]/g, "");
    if (step || m.kind !== "tool") continue;
    const tool = m.meta?.tool;
    if (isStepTool(tool) || tool === "AskUserQuestion") continue;
    step = m.content.replace(/^mcp__tab__/, "tab: ").split("\n")[0];
  }
  if (!running && !todos.length) return null;
  const done = todos.filter((t) => t.status === "completed").length;
  const active = plan.find((t) => t.status === "in_progress");
  const busy = todos.some((t) => t.status === "in_progress");
  const now = writing ? "Writing a reply" : note || step || "Working";
  const label = active ? active.activeForm || active.content : now;
  const count = plan.length ? `${done} of ${plan.length}` : activity.length ? `${activity.length} tool call${activity.length > 1 ? "s" : ""}` : "";
  const open = todos.length > 0 && !collapsed;
  return (
    <div className={cx("progress-bar", running && "running")}>
      {open ? (
        <button className="progress-head" onClick={() => setCollapsed(true)} aria-expanded>
          {!running && <Icon name="check" size={12} />}
          <span className="progress-label">{running ? (plan.length ? "Steps" : "This turn") : "Done"}</span>
          {count && <span className="progress-count">{count}</span>}
        </button>
      ) : (
        <button className="progress-head" onClick={() => todos.length && setCollapsed(false)} aria-expanded={false}>
          {running ? <span className="dots"><i /><i /><i /></span> : <Icon name="check" size={12} />}
          <span className="progress-label">{running ? label : "Done"}</span>
          {count && <span className="progress-count">{count}</span>}
        </button>
      )}
      {open && (
        <ul className={cx("progress-list", !plan.length && "activity")}>
          {earlier > 0 && <li className="progress-earlier">{earlier} earlier</li>}
          {todos.map((t, i) => (
            <li key={i} className={t.status}>
              <span className={cx("step-mark", t.status, !running && "still")}>{t.status === "completed" && <Icon name="check" size={9} />}</span>
              <span>{t.status === "in_progress" && running ? t.activeForm || t.content : t.content}</span>
            </li>
          ))}
          {running && !busy && (
            <li className="progress-now">
              <span className="dots"><i /><i /><i /></span>
              <span>{now}</span>
            </li>
          )}
        </ul>
      )}
      {running && busy && (note || step) && <div className="progress-step">{note || step}</div>}
    </div>
  );
}

/** Tool calls the person sees as rows (not step updates or questions, which have their own cards). */
const isRow = (m: SessionMessage) => m.kind === "tool" && !isStepTool(m.meta?.tool) && m.meta?.tool !== "AskUserQuestion";

/**
 * The agent's text between tool calls ("Reading the config next…") is its
 * working-out, not its answer: it folds into the work group around it, so
 * a turn reads as the person's message, one line of work, and the reply.
 * Text stays out when nothing but its answer follows it. While the agent
 * works, all of this turn's text folds (it can't be told apart yet), except
 * its reply to a message sent mid-turn, which the person is waiting on.
 */
function foldedTexts(messages: SessionMessage[], running: boolean): Set<number> {
  const out = new Set<number>();
  let laterWork = running; // walking back: real work comes after this point in the turn
  for (let i = messages.length - 1; i >= 0; i--) {
    const m = messages[i];
    if (m.role === "user") {
      // The reply to a steer: its first text stays where the person can see it.
      if (m.meta?.steer) for (let j = i + 1; j < messages.length && messages[j].role !== "user"; j++) {
        if (messages[j].role === "assistant" && messages[j].kind === "text") { out.delete(messages[j].id); break; }
      }
      laterWork = false;
      continue;
    }
    if (isRow(m)) laterWork = true;
    else if (m.role === "assistant" && m.kind === "text" && laterWork) out.add(m.id);
  }
  return out;
}

interface Turn { messages: SessionMessage[]; files: string[] }

/**
 * The chat by turn (each person's message and what the agent did after it),
 * with the files the agent showed in that turn: shown together at the end of
 * the turn, not wherever its working-out named them.
 */
function splitTurns(messages: SessionMessage[], base: string | null): Turn[] {
  const turns: Turn[] = [];
  const attached = new Set<string>();
  for (const m of messages) if (m.role === "user" && Array.isArray(m.meta?.attachments)) for (const p of m.meta.attachments) attached.add(p);
  const seen = new Set<string>();
  for (const m of messages) {
    if (m.role === "user" || !turns.length) turns.push({ messages: [], files: [] });
    const t = turns[turns.length - 1];
    t.messages.push(m);
    if (m.role !== "assistant" || m.kind !== "text") continue;
    for (const p of filesInText(m.content, base)) {
      if (seen.has(p) || attached.has(p) || !isShowable(p)) continue;
      seen.add(p);
      t.files.push(p);
    }
  }
  return turns;
}

/** What the agent showed in a turn: images in full, documents as cards that open on the stage. */
function TurnFiles({ paths }: { paths: string[] }) {
  const stage = useStage();
  const images = paths.filter(isImagePath);
  const docs = paths.filter((p) => !isImagePath(p));
  return (
    <div className="turn-files">
      {images.map((p) => <ImageThumb key={p} src={p} list={images} />)}
      {docs.length > 0 && (
        <div className="turn-docs">
          {docs.map((p, i) => (
            <button
              key={p}
              className="file-tile turn-doc"
              title={p}
              onClick={(e) => {
                if (e.altKey || !stage) return void api.fileOpen(p, e.altKey).catch(() => {});
                stage.open(docs, i);
              }}
            >
              <FilePreview path={p} size="sm" />
              <span className="file-tile-name">{fileName(p)}</span>
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

/**
 * A turn with runs of back-to-back tool calls, and the agent's working-out
 * between them, gathered into one group each, so a long stretch of work
 * reads as one line, not fifty. Step updates and paired results inside a
 * run don't break it.
 */
function groupTools(messages: SessionMessage[], paired: Set<SessionMessage>, folded: Set<number>): (SessionMessage | SessionMessage[])[] {
  const out: (SessionMessage | SessionMessage[])[] = [];
  let run: SessionMessage[] | null = null;
  for (const m of messages) {
    if (m.kind === "tool_result" && paired.has(m)) continue;
    if (m.kind === "tool" && isStepTool(m.meta?.tool)) continue;
    if (m.kind === "suggestion") continue;
    if (isRow(m) || folded.has(m.id)) {
      if (!run) out.push((run = []));
      run.push(m);
      continue;
    }
    run = null;
    out.push(m);
  }
  return out;
}

/** A run of work: one line saying how many tool calls and which, opening to each call and the agent's notes between them. */
function ToolGroup({ calls: run, results }: { calls: SessionMessage[]; results: Map<number, SessionMessage> }) {
  const calls = run.filter((m) => m.kind === "tool");
  if (run.length === 1 && calls.length === 1) return <ToolRow m={calls[0]} result={results.get(calls[0].id)} />;
  const names = [...new Set(calls.map((c) => toolName(c.meta?.tool ?? c.content)))];
  const failed = calls.filter((c) => results.get(c.id)?.meta?.is_error).length;
  const note = run.find((m) => m.kind === "text");
  return (
    <details className={cx("msg-tool msg-tool-group", failed > 0 && "failed")}>
      <summary>
        <Icon name="terminal" size={12} />
        <span className="msg-tool-name">
          {calls.length
            ? <>{calls.length} tool call{calls.length > 1 ? "s" : ""} · {names.slice(0, 4).join(", ")}{names.length > 4 ? ` +${names.length - 4}` : ""}</>
            : note?.content.split("\n")[0]}
        </span>
        {failed > 0 && <span className="msg-tool-flag">{failed} failed</span>}
      </summary>
      <div className="msg-tool-list">
        {run.map((c) => c.kind === "text"
          ? <div key={c.id} className="msg-thought"><Markdown text={c.content} /></div>
          : <ToolRow key={c.id} m={c} result={results.get(c.id)} />)}
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

/** Asks the composer to open `/rewind` at one of the person's messages. */
export function openRewind(sessionId: string, messageId: number) {
  window.dispatchEvent(new CustomEvent("tab:rewind", { detail: { sessionId, messageId } }));
}

function MessageView({ m, result, onSend, rewind }: { m: SessionMessage; result?: SessionMessage; onSend?: (text: string) => void; rewind?: string }) {
  if (m.kind === "suggestion") return null;
  if (m.kind === "shell") return <ShellRun m={m} />;
  if (m.role === "user") {
    const images: string[] = Array.isArray(m.meta?.attachments) ? m.meta.attachments : [];
    return (
      <div className="msg msg-user">
        {images.length > 0 && <Attachments paths={images} size={96} />}
        {/```|\]\(/.test(m.content) ? <Markdown text={m.content} /> : m.content}
        {rewind && m.meta?.agent_uuid && (
          <button className="msg-rewind" onClick={() => openRewind(rewind, m.id)} title="Rewind: go back to before this message (the conversation, the code, or both)" aria-label="Rewind to before this message">
            <Icon name="refresh" size={11} /> Rewind
          </button>
        )}
      </div>
    );
  }
  if (m.kind === "secret_request") return m.meta?.superseded ? null : <SecretCard m={m} onSend={onSend} />;
  if (m.kind === "compacted") return <CompactedDivider m={m} />;
  if (m.kind === "wake") return <div className="msg-note msg-wake"><Icon name="clock" size={12} /> {m.content}</div>;
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

/** A command the person ran with `!`, and what it printed. The agent sees both with their next message. */
function ShellRun({ m }: { m: SessionMessage }) {
  const output: string = m.meta?.output ?? "";
  const code: number | null = m.meta?.exit_code ?? null;
  return (
    <div className="msg-shell" title="Ran in the chat's folder. The agent sees this with your next message.">
      <div className="msg-shell-cmd">
        <span className="msg-shell-bang">!</span>
        <code>{m.content}</code>
        {code !== 0 && <span className="msg-tool-flag">{code == null ? "stopped" : `exit ${code}`}</span>}
      </div>
      {output ? <pre>{output}</pre> : <div className="msg-tool-empty">No output</div>}
    </div>
  );
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
  if (approval.tool === "tab_vault") return <VaultApproval approval={approval} />;
  return <ToolApproval approval={approval} />;
}

/** An agent wants something from the vault: unlock it, release fields, or share them with a thread. */
function VaultApproval({ approval }: { approval: Approval }) {
  const input = approval.input as { kind: "unlock" | "release" | "request"; why?: string; entry?: string; fields?: (string | { label: string; masked: string })[]; file?: boolean };
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const allow = async () => {
    setBusy(true);
    setError(null);
    try {
      // Approving opens the vault here first (Touch ID / Windows Hello).
      const st = await api.vaultStatus();
      if (st.state === "locked") await api.vaultUnlock();
      else if (st.state !== "open") throw new Error("This device can't open the vault. Answer on the computer running this chat.");
      await api.approvalResolve(approval.id, "allow");
    } catch (e) {
      setError(String(e));
      setBusy(false);
    }
  };
  const deny = async () => (setBusy(true), await api.approvalResolve(approval.id, "deny"));
  const fields = input.fields ?? [];
  return (
    <div className="approval vault-approval">
      <div className="approval-head"><Icon name="lock" size={14} /> {approval.summary}</div>
      {fields.length > 0 && (
        <div className="vault-approval-fields">
          {fields.map((f) => typeof f === "string"
            ? <span key={f} className="vault-chip">{f}</span>
            : <span key={f.label} className="vault-chip">{f.label} <code>{f.masked}</code></span>)}
          {input.file && <span className="vault-chip">file text</span>}
        </div>
      )}
      {input.why && <p className="vault-approval-why"><span className="subtle">Why:</span> {input.why}</p>}
      {input.kind !== "unlock" && <p className="subtle vault-approval-note">Only this one answer gets the values; the saved chat shows them masked. Logged in the vault's activity.</p>}
      {error && <div className="chat-error"><Icon name="x-circle" size={14} /> {error}</div>}
      <div className="toolbar">
        <Button size="sm" variant="primary" icon="check" disabled={busy} onClick={allow}>{input.kind === "unlock" ? "Unlock" : input.kind === "release" ? "Show to the coordinator" : "Share"}</Button>
        <Button size="sm" variant="ghost" icon="x" disabled={busy} onClick={deny}>Deny</Button>
      </div>
    </div>
  );
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
function BarPicker({ value, options, onChange, title, className, openSignal }: { value: string; options: { value: string; label: string; help?: string }[]; onChange: (v: string) => void; title: string; className?: string; openSignal?: number }) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLSpanElement>(null);
  useEffect(() => void (openSignal && setOpen(true)), [openSignal]);
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
 * chat starts any harness can be picked, and the pick becomes what new chats
 * start on. After, the chat keeps its own model on its harness: changing it
 * applies from the next message to this chat only.
 */
function ChatModelPicker({ chat, openSignal }: { chat: Chat | undefined; openSignal?: number }) {
  const { data: config } = useConfig();
  const { data: models = [] } = useModels();
  const qc = useQueryClient();
  // A switch on a started chat waits here until the person confirms it.
  const [pending, setPending] = useState<string | null | undefined>(undefined);
  const [busy, setBusy] = useState(false);
  const session = chat?.session;
  const agent = session?.agent ?? config?.agent.kind;
  if (!config || !agent) return null;
  // Older chats have no model of their own until their next message.
  const own = session?.model;
  const model = own != null ? own || null : modelFor(config.agent, agent);
  const label = (m: string | null) => (m ? models.find((x) => x.agent === agent && x.id === m)?.label ?? m : "the default model");
  const confirm = async () => {
    if (!session || pending === undefined) return;
    setBusy(true);
    try {
      await api.setSessionModel(session.id, pending);
      setPending(undefined);
    } finally {
      setBusy(false);
    }
  };
  return (
    <>
      <ModelMenu
        value={{ agent, model }}
        lock={session ? agent : undefined}
        openSignal={openSignal}
        title={session ? "Model for this chat, from the next message" : "Model for this chat and new ones"}
        onChange={async (p) => {
          if (session) setPending(p.model);
          else qc.setQueryData(keys.config, await api.setAgentModel(p.model, p.agent, true));
        }}
      />
      {pending !== undefined && (
        <div
          className="confirm-overlay"
          role="dialog"
          aria-label="Switch model"
          onMouseDown={(e) => e.target === e.currentTarget && !busy && setPending(undefined)}
          onKeyDown={(e) => e.key === "Escape" && !busy && setPending(undefined)}
        >
          <div className="confirm-dialog">
            <div className="confirm-dialog-title">Switch this chat to {label(pending)}?</div>
            <div className="subtle">
              It's on {label(model)} now. The switch applies from your next message, to this chat only. The agent
              starts without a warm cache, so that reply can cost more.
            </div>
            <div className="confirm-dialog-foot">
              <Button variant="ghost" size="sm" onClick={() => setPending(undefined)} disabled={busy}>Cancel</Button>
              <Button variant="primary" size="sm" autoFocus onClick={() => void confirm()} disabled={busy}>Switch model</Button>
            </div>
          </div>
        </div>
      )}
    </>
  );
}

/** The modes the chat's harness offers (from its metadata). */
function usePermissionModes(chat: Chat | undefined) {
  const { data: config } = useConfig();
  const { data: harnesses } = useHarnesses();
  const qc = useQueryClient();
  const session = chat?.session;
  const agent = session?.agent ?? config?.agent.kind;
  const modes = harnesses?.find((h) => h.id === agent)?.capabilities.permission_modes.map((m) => ({ ...m, help: m.help ?? undefined })) ?? [];
  // A started chat has its own; before that, it's what new chats start in.
  const current = session?.permission_mode || config?.agent.permission_mode || "default";
  const set = async (v: string) => {
    if (session) await api.setSessionMode(session.id, v);
    else qc.setQueryData(keys.config, await api.setAgentMode(v));
  };
  return { ready: !!config && modes.length > 0, modes, current, set };
}

/**
 * The chat's permission mode. On a started chat it's that chat's own (a
 * Claude Code turn under way switches at once); before, it sets the mode
 * new chats start in. Shift+Tab in the box cycles it, as in Claude Code.
 */
function ModePicker({ chat, openSignal }: { chat: Chat | undefined; openSignal?: number }) {
  const { ready, modes, current, set } = usePermissionModes(chat);
  if (!ready) return null;
  const options = modes.some((m) => m.value === current) ? modes : [...modes, { value: current, label: current }];
  const title = chat?.session ? "Permission mode for this chat" : "Permission mode for this chat and new ones";
  return <BarPicker value={current} options={options} title={title} className={cx(current === "auto" && "mode-auto")} openSignal={openSignal} onChange={(v) => void set(v)} />;
}

/** What Claude Code's built-in commands do, for an agent that reported only their names. */
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
// Terminal-only commands that do nothing useful in a headless turn and that
// tab has no answer for. The ones it does answer are in `tabCommands`.
const interactiveOnly = new Set(["vim", "exit", "quit", "theme", "terminal-setup", "ide", "statusline", "bug", "feedback", "privacy-settings", "output-style", "install-github-app", "migrate-installer", "add-dir", "export", "color", "focus", "heapdump", "import"]);
const fallbackSlash = ["compact", "context", "cost", "mcp", "model", "status", "init", "review", "pr-comments", "security-review"];

/** The `/` menu's groups, in order. */
const slashGroups: Record<SlashKind | "tab", string> = { builtin: "Built in", tab: "Answered by tab", command: "Your commands", skill: "Skills", mcp: "MCP prompts" };
const groupOrder = Object.keys(slashGroups) as (SlashKind | "tab")[];
const harnessNames: Record<AgentKind, string> = { claude_code: "Claude Code", opencode: "OpenCode", codex: "Codex", pi: "Pi" };
/** Headings for argument values, by where they come from. */
const argGroups: Record<ArgSource, string> = { pr: "Pull requests", branch: "Branches", path: "Files", model: "Models", agent: "Subagents" };

/** One row of the `/` menu: a command, or a value for the argument being typed. */
interface SlashRow { value: string; label: string; detail?: string; hint?: string; group?: string; tab?: boolean }

/** Open pull requests to offer for a `<pr#>` argument: yours, and the ones in the queue. Those in the chat's repo come first. */
async function openPulls(repoName: string): Promise<SlashRow[]> {
  const [mine, items] = await Promise.all([api.githubMyPulls().catch(() => []), api.listItems(["open"]).catch(() => [] as QueueItem[])]);
  const seen = new Set<string>();
  const rows: (SlashRow & { here: boolean })[] = [];
  const add = (repo: string, n: number, title: string) => {
    if (seen.has(`${repo}#${n}`)) return;
    seen.add(`${repo}#${n}`);
    rows.push({ value: String(n), label: `#${n}`, detail: `${repo} · ${title}`, here: repo.split("/").pop()?.toLowerCase() === repoName });
  };
  for (const p of mine) add(p.repo, p.number, p.summary.title);
  for (const i of items) {
    const m = /github\.com\/([^/]+\/[^/]+)\/pull\/(\d+)/.exec(i.url ?? "");
    if (m) add(m[1], Number(m[2]), i.title);
  }
  return rows.sort((a, b) => Number(b.here) - Number(a.here));
}

/**
 * The `/` menu. While the command is typed it lists Claude Code's commands,
 * custom commands, skills and MCP prompts, as the agent described them when
 * it last started, grouped; tab's own answers are marked "tab". Once a
 * command is picked, its argument hint shows as ghost text and the values
 * tab knows for the next argument (enum words, PRs, branches, paths, models,
 * subagents) are offered.
 */
function useSlashMenu(text: string, cwd: string | undefined, agent: AgentKind | undefined) {
  const { data } = useQuery({ queryKey: ["slash-commands", agent], queryFn: () => api.slashCommands(agent ?? null), enabled: !!agent, staleTime: 60_000 });
  // Each harness has its own commands; the built-in names tab knows are Claude Code's.
  const claude = agent === "claude_code";
  const [index, setIndex] = useState(0);
  const [moved, setMoved] = useState(false);
  const [dismissed, setDismissed] = useState<string | null>(null);
  const commands: SlashCommand[] = data?.length || !claude ? data ?? [] : fallbackSlash.map((name) => ({ name, description: "", argument_hint: "", aliases: [], kind: "builtin" }));
  const find = (n: string) => commands.find((c) => c.name.toLowerCase() === n || c.aliases.some((a) => a.toLowerCase() === n));

  // `/name`, or `/name args…` once a space follows.
  const typing = /^\/([\w:.-]*)$/.exec(text);
  const withArgs = /^\/([\w:.-]+)\s([^\n]*)$/.exec(text);
  const q = typing?.[1].toLowerCase() ?? "";
  const cmd = withArgs && !(withArgs[1].toLowerCase() in tabCommands) ? find(withArgs[1].toLowerCase()) : undefined;
  const { args, word } = splitArgs(withArgs?.[2] ?? "");
  const slots = useMemo(() => openSlots(parseHint(cmd?.argument_hint ?? ""), args), [cmd?.argument_hint, args.join(" ")]);
  const wants = new Set(slots.flatMap((s) => s.sources));

  const repoName = cwd?.split(/[\\/]/).filter(Boolean).pop()?.toLowerCase() ?? "";
  const pulls = useQuery({ queryKey: ["slash-prs", repoName], queryFn: () => openPulls(repoName), enabled: wants.has("pr"), staleTime: 60_000 });
  const branches = useQuery({ queryKey: ["slash-branches", cwd], queryFn: () => api.slashArgValues(cwd!, "branch", ""), enabled: wants.has("branch") && !!cwd, staleTime: 30_000 });
  // A folder's entries, fetched once per folder and filtered as you type.
  const folder = word.slice(0, word.lastIndexOf("/") + 1) + (word.slice(word.lastIndexOf("/") + 1).startsWith(".") ? "." : "");
  const paths = useQuery({ queryKey: ["slash-paths", cwd, folder], queryFn: () => api.slashArgValues(cwd!, "path", folder), enabled: wants.has("path") && !!cwd, staleTime: 10_000 });
  const { data: models = [] } = useModels();
  // Subagents are Claude Code's.
  const agents = useQuery({ queryKey: ["subagents"], queryFn: api.subagents, enabled: wants.has("agent") && claude, staleTime: 60_000 });

  let rows: SlashRow[] = [];
  let ghost = "";
  if (typing) {
    const hit = (c: { name: string; aliases: string[] }) => [c.name, ...c.aliases].some((n) => n.toLowerCase().includes(q));
    const starts = (c: { name: string; aliases: string[] }) => [c.name, ...c.aliases].some((n) => n.toLowerCase().startsWith(q));
    const agentRows = commands
      .filter((c) => !(claude && interactiveOnly.has(c.name)) && !(c.name in tabCommands) && !c.description.startsWith("(removed)") && hit(c))
      .map((c) => ({ c, kind: c.kind as SlashKind | "tab", row: { value: c.name, label: c.name, detail: c.description || (claude ? slashDescriptions[c.name] : undefined), hint: c.argument_hint } }));
    const tabRows = Object.entries(tabCommands)
      .filter(([name]) => name.includes(q))
      .map(([name, t]) => ({ c: { name, aliases: [] }, kind: "tab" as const, row: { value: name, label: name, detail: t.description, tab: true } }));
    rows = [...agentRows, ...tabRows]
      .sort((a, b) => groupOrder.indexOf(a.kind) - groupOrder.indexOf(b.kind) || Number(!starts(a.c)) - Number(!starts(b.c)))
      .slice(0, 60)
      .map(({ kind, row }) => ({ ...row, group: kind === "builtin" && agent ? harnessNames[agent] : slashGroups[kind] }));
    // The whole name typed: what it takes, before the space.
    const exact = q && !(q in tabCommands) ? find(q) : undefined;
    if (exact?.argument_hint) ghost = ` ${exact.argument_hint}`;
  } else if (cmd) {
    const w = word.toLowerCase();
    const seen = new Set<string>();
    const offer = (r: SlashRow) => {
      const match = r.value.toLowerCase().startsWith(w) || (w.length > 1 && !!r.detail?.toLowerCase().includes(w));
      if (match && !seen.has(r.value)) (seen.add(r.value), rows.push(r));
    };
    for (const s of slots) {
      for (const v of s.values) offer({ value: v, label: v, group: "Options" });
      for (const src of s.sources) {
        const group = argGroups[src];
        if (src === "pr") pulls.data?.forEach((r) => offer({ ...r, group }));
        if (src === "branch") branches.data?.forEach((b) => offer({ value: b, label: b, group }));
        if (src === "path") paths.data?.forEach((p) => offer({ value: p, label: p, group }));
        if (src === "model") models.filter((m) => m.agent === "claude_code").forEach((m) => offer({ value: m.id, label: m.id, detail: m.label, group }));
        if (src === "agent") agents.data?.forEach((a) => offer({ value: a.name, label: a.name, detail: a.description, group }));
      }
    }
    // Each group together, in the order the hint first offers it.
    const order = [...new Set(rows.map((r) => r.group))];
    rows.sort((a, b) => order.indexOf(a.group) - order.indexOf(b.group));
    rows = rows.slice(0, 40);
    if (!word && slots.length) ghost = slots.map((s) => s.text).join(" ");
  }

  const open = rows.length > 0 && dismissed !== text;
  const mode = typing ? "command" : "arg";
  useEffect(() => (setIndex(0), setMoved(false)), [q, word, cmd?.name, args.length]);
  /** Fills in the picked row: a command's name, or the argument being typed. */
  const fill = (row: SlashRow) => {
    if (mode === "command") return row.value;
    const before = text.slice(0, text.length - word.length);
    // A folder goes on to its entries; anything else is a whole argument.
    return before + row.value + (row.value.endsWith("/") ? "" : " ");
  };
  const handleKey = (e: React.KeyboardEvent, pick: (row: SlashRow) => void) => {
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      const d = e.key === "ArrowDown" ? 1 : -1;
      setIndex((i) => (i + d + rows.length) % rows.length);
      setMoved(true);
      return true;
    }
    if (e.key === "Tab" || (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing)) {
      const r = rows[Math.min(index, rows.length - 1)];
      if (e.key === "Enter") {
        // A full command or value already typed sends on Enter, and so does
        // Enter after a space unless the person went into the list.
        if (mode === "command" && (r.value.toLowerCase() === q || find(q)?.name === r.value)) return false;
        if (mode === "arg" && (r.value.toLowerCase() === word.toLowerCase() || (!word && !moved))) return false;
      }
      e.preventDefault();
      pick(r);
      return true;
    }
    if (e.key === "Escape") {
      e.preventDefault();
      setDismissed(text);
      return true;
    }
    return false;
  };
  return { open, mode, rows, index, setIndex, handleKey, fill, ghost: dismissed === text && mode === "command" ? "" : ghost, close: () => setDismissed(null) };
}

/** The `/` menu's list, with a heading where its group changes. */
function SlashMenu({ slash, onPick }: { slash: { rows: SlashRow[]; index: number; setIndex: (i: number) => void; mode: string }; onPick: (row: SlashRow) => void }) {
  const list = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => {
    list.current?.querySelector(".active")?.scrollIntoView({ block: "nearest" });
  }, [slash.index]);
  return (
    <div ref={list} className="slash-menu" role="listbox">
      {slash.rows.map((r, i) => (
        <Fragment key={r.value}>
          {r.group && r.group !== slash.rows[i - 1]?.group && <div className="slash-group">{r.group}</div>}
          <button role="option" aria-selected={i === slash.index} className={cx(i === slash.index && "active")} title={r.detail} onMouseDown={(e) => (e.preventDefault(), onPick(r))} onMouseEnter={() => slash.setIndex(i)}>
            {slash.mode === "mention" && <Icon name={r.value.endsWith("/") ? "folder" : "file"} size={12} />}
            <span className="slash-name">{slash.mode === "command" ? `/${r.label}` : r.label}</span>
            {r.hint && <span className="slash-hint">{r.hint}</span>}
            {r.detail && <span className="slash-desc">{r.detail}</span>}
            {r.tab && <span className="slash-tab" title="tab answers this itself">tab</span>}
          </button>
        </Fragment>
      ))}
    </div>
  );
}

/** An `@` mention as typed, ending at the caret: `@src/Ch`. */
const MENTION = /(^|\s)@([^\s@"]*)$/;

/**
 * The `@` menu: files and folders in the chat's folder, best match for
 * what's typed after `@` first, as in Claude Code. Picking one puts its
 * path in the message; a folder goes on to its entries.
 */
function useMentionMenu(text: string, caret: number, cwd: string | undefined) {
  const before = text.slice(0, caret);
  const q = MENTION.exec(before)?.[2] ?? null;
  const [index, setIndex] = useState(0);
  const [dismissed, setDismissed] = useState<string | null>(null);
  const { data = [] } = useQuery({
    queryKey: ["mentions", cwd, q],
    queryFn: () => api.chatMentions(cwd!, q!),
    enabled: q !== null && !!cwd,
    staleTime: 10_000,
    placeholderData: (prev) => prev,
  });
  const rows: SlashRow[] = q === null ? [] : data.map((p) => ({ value: p, label: p }));
  const open = rows.length > 0 && dismissed !== before;
  useEffect(() => setIndex(0), [q]);
  /** The text with the picked path in place of what was typed, and where the caret goes. */
  const fill = (row: SlashRow) => {
    const start = caret - (q ?? "").length;
    const path = /\s/.test(row.value) ? `"${row.value}"` : row.value;
    const add = row.value.endsWith("/") ? path : `${path} `;
    const after = text.slice(caret).replace(/^\S*/, "");
    const rest = add.endsWith(" ") ? after.replace(/^ /, "") : after;
    return { text: text.slice(0, start) + add + rest, caret: start + add.length };
  };
  const handleKey = (e: React.KeyboardEvent, pick: (row: SlashRow) => void) => {
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      const d = e.key === "ArrowDown" ? 1 : -1;
      setIndex((i) => (i + d + rows.length) % rows.length);
      return true;
    }
    if (e.key === "Tab" || (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing)) {
      const r = rows[Math.min(index, rows.length - 1)];
      // Already typed out in full: Enter sends.
      if (e.key === "Enter" && r.value === q) return false;
      e.preventDefault();
      pick(r);
      return true;
    }
    if (e.key === "Escape") {
      e.preventDefault();
      setDismissed(before);
      return true;
    }
    return false;
  };
  return { open, mode: "mention", rows, index, setIndex, handleKey, fill };
}

const HISTORY_KEY = "tab.composerHistory";

/** What the person sent from any chat, newest first, for ↑ in the composer. */
function sentHistory(): string[] {
  try {
    const h = JSON.parse(localStorage.getItem(HISTORY_KEY) ?? "[]");
    return Array.isArray(h) ? h.filter((x) => typeof x === "string") : [];
  } catch {
    return [];
  }
}

function remember(text: string) {
  try {
    localStorage.setItem(HISTORY_KEY, JSON.stringify([text, ...sentHistory().filter((x) => x !== text)].slice(0, 100)));
  } catch {
    // History is a convenience; a full or blocked storage just skips it.
  }
}

export function Composer({ chat, onSend, onShell, placeholder, autoFocus, extra }: { chat: Chat | undefined; onSend: (text: string, attachments: string[]) => void; onShell?: (command: string) => void; placeholder: string; autoFocus?: boolean; extra?: React.ReactNode }) {
  const { data: config } = useConfig();
  const [text, setText] = useState("");
  const [caret, setCaret] = useState(0);
  // Where ↑/↓ is in the history, and what was typed before it started.
  const [recall, setRecall] = useState<{ i: number; draft: string } | null>(null);
  // The first Esc of a possible Esc Esc, and leaving the box if none follows.
  const lastEsc = useRef<{ at: number; leave: number } | null>(null);
  const mode = usePermissionModes(chat);
  const [images, setImages] = useState<string[]>([]);
  const [uploading, setUploading] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const ref = useRef<HTMLTextAreaElement>(null);
  const file = useRef<HTMLInputElement>(null);
  const running = !!chat?.running;
  const last = chat?.messages[chat.messages.length - 1];
  const suggestion = !running && last?.kind === "suggestion" && config?.agent.suggest_replies !== false ? last.content : "";
  const slash = useSlashMenu(text, chat?.session?.cwd, chat?.session?.agent ?? config?.agent.kind ?? undefined);
  const shellMode = !!onShell && text.startsWith("!");
  const mention = useMentionMenu(text, caret, chat?.session?.cwd ?? config?.agent.workdir ?? undefined);
  const [panel, setPanel] = useState<Panel | null>(null);
  const [rewindTo, setRewindTo] = useState<number | null>(null);
  // "Rewind" on a message in this chat opens the panel there.
  useEffect(() => {
    const open = (e: Event) => {
      const d = (e as CustomEvent<{ sessionId: string; messageId: number }>).detail;
      if (d.sessionId !== chat?.session?.id) return;
      setRewindTo(d.messageId);
      setPanel("rewind");
    };
    window.addEventListener("tab:rewind", open);
    return () => window.removeEventListener("tab:rewind", open);
  }, [chat?.session?.id]);
  const onRewound = (r: { text: string; attachments: string[] }, conversation: boolean) => {
    setRewindTo(null);
    if (conversation) {
      setText(r.text);
      setImages(r.attachments);
    }
    ref.current?.focus();
  };
  const pickMention = (row: SlashRow) => {
    const next = mention.fill(row);
    setText(next.text);
    setCaret(next.caret);
    requestAnimationFrame(() => ref.current?.setSelectionRange(next.caret, next.caret));
    ref.current?.focus();
  };
  /** Past messages for ↑: this chat's newest first, then what was sent elsewhere. */
  const history = () => {
    const own = (chat?.messages ?? [])
      .filter((m) => (m.role === "user" && m.kind === "text") || m.kind === "shell")
      .map((m) => (m.kind === "shell" ? `!${m.content}` : m.content))
      .reverse();
    return [...new Set([...own, ...sentHistory()].filter((t) => t.trim()))];
  };
  const [picker, setPicker] = useState<{ which: "model" | "mode"; n: number }>({ which: "model", n: 0 });
  // A tab command runs at once; the agent's own get a space for arguments.
  const pickSlash = (row: SlashRow) => {
    slash.close();
    if (slash.mode === "command" && tabCommands[row.value]) return runCommand(`/${row.value}`);
    setText(slash.mode === "command" ? `/${row.value} ` : slash.fill(row));
    ref.current?.focus();
  };
  /** Answers an interactive command in tab. False when it's for the agent. */
  const runCommand = (t: string) => {
    const c = tabCommand(t);
    if (!c) return false;
    setText("");
    const a = c.action;
    if ("panel" in a) (setRewindTo(null), setPanel(a.panel));
    else if ("picker" in a) (setPanel(null), setPicker((p) => ({ which: a.picker, n: p.n + 1 })));
    else (setPanel(null), a.go());
    return true;
  };

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
    setRecall(null);
    if (shellMode) {
      const command = t.slice(1).trim();
      if (!command) return;
      remember(t);
      setText("");
      setError(null);
      onShell!(command);
      return;
    }
    if (!images.length && runCommand(t)) return;
    if (t) remember(t);
    setText("");
    setImages([]);
    setError(null);
    onSend(t, images);
  };

  /** ↑ on the first line goes back through what was sent, ↓ on the last comes forward. */
  const recallKey = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    const el = e.currentTarget;
    if (e.shiftKey || e.altKey || e.metaKey || e.ctrlKey || el.selectionStart !== el.selectionEnd) return false;
    if (e.key === "ArrowUp" && !text.slice(0, el.selectionStart).includes("\n")) {
      const past = history();
      const i = (recall?.i ?? -1) + 1;
      if (i >= past.length) return false;
      e.preventDefault();
      setRecall({ i, draft: recall?.draft ?? text });
      setText(past[i]);
      return true;
    }
    if (e.key === "ArrowDown" && recall && !text.slice(el.selectionEnd).includes("\n")) {
      e.preventDefault();
      const i = recall.i - 1;
      setText(i < 0 ? recall.draft : history()[i] ?? recall.draft);
      setRecall(i < 0 ? null : { ...recall, i });
      return true;
    }
    return false;
  };

  /**
   * Esc Esc, as in Claude Code: clears what's typed, or with nothing typed,
   * opens /rewind. A single Esc still leaves the box, a moment later.
   */
  const escKey = (e: React.KeyboardEvent) => {
    if (e.key !== "Escape" || panel || (!text && !chat?.session)) return;
    e.preventDefault();
    const first = lastEsc.current;
    if (!first || Date.now() - first.at > 800) {
      const leave = window.setTimeout(() => ((lastEsc.current = null), ref.current?.blur()), 800);
      lastEsc.current = { at: Date.now(), leave };
      return;
    }
    window.clearTimeout(first.leave);
    lastEsc.current = null;
    // The panel that opens listens for Esc to close; this one isn't for it.
    e.stopPropagation();
    if (text) {
      remember(text);
      setText("");
      setRecall(null);
    } else if (chat?.session) {
      setRewindTo(null);
      setPanel("rewind");
    }
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
      <AgentNotice messages={chat?.messages} starting={!chat?.session && !chat?.running} />
      {panel && <SlashPanel panel={panel} chat={chat} rewind={{ to: rewindTo, onRewound }} onClose={() => (setPanel(null), setRewindTo(null), ref.current?.focus())} />}
      {(images.length > 0 || uploading > 0) && (
        <div className="composer-images">
          <Attachments paths={images} size={56} onRemove={(p) => setImages((xs) => xs.filter((x) => x !== p))} />
          {uploading > 0 && <span className="thumb thumb-busy" style={{ width: 56, height: 56 }}><Icon name="refresh" size={14} /></span>}
        </div>
      )}
      <div className={cx("composer-input", shellMode && "shell-mode")}>
      {slash.ghost && !text.includes("\n") && (
        <div className="composer-ghost" aria-hidden><span>{text}</span>{slash.ghost}</div>
      )}
      <textarea
        ref={ref}
        rows={1}
        value={text}
        placeholder={suggestion || (running ? "Steer the agent while it works…" : placeholder)}
        onChange={(e) => (setText(e.target.value), setCaret(e.target.selectionStart), setRecall(null))}
        onSelect={(e) => setCaret(e.currentTarget.selectionStart)}
        onPaste={(e) => {
          const files = [...e.clipboardData.files];
          if (!files.length) return;
          e.preventDefault();
          void add(files);
        }}
        onKeyDown={(e) => {
          if (mention.open && mention.handleKey(e, pickMention)) return;
          if (slash.open && slash.handleKey(e, pickSlash)) return;
          if (!mention.open && !slash.open && recallKey(e)) return;
          // Shift+Tab cycles the permission mode, as in Claude Code.
          if (e.key === "Tab" && e.shiftKey && mode.ready) {
            e.preventDefault();
            const i = mode.modes.findIndex((m) => m.value === mode.current);
            return void mode.set(mode.modes[(i + 1) % mode.modes.length].value);
          }
          escKey(e);
          // Tab takes the suggested reply; Enter then sends it.
          if (e.key === "Tab" && !e.shiftKey && !text && suggestion) return (e.preventDefault(), setText(suggestion));
          if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) (e.preventDefault(), submit());
        }}
      />
      </div>
      {mention.open ? <SlashMenu slash={mention} onPick={pickMention} /> : slash.open && <SlashMenu slash={slash} onPick={pickSlash} />}
      {shellMode && <div className="composer-suggest subtle">Shell: runs in the chat's folder. The agent sees the output with your next message.</div>}
      {error && <div className="composer-error">{error}</div>}
      {suggestion && !text && <div className="composer-suggest subtle"><kbd>Tab</kbd> to use the suggested reply, then <kbd>Enter</kbd> to send</div>}
      <div className="composer-foot">
        <span className="subtle composer-agent">
          <button className="bar-icon" onClick={() => file.current?.click()} title="Attach files or images (or paste or drop them)" aria-label="Attach files"><Icon name="plus" size={14} /></button>
          <input ref={file} type="file" multiple hidden onChange={(e) => (void add([...(e.target.files ?? [])]), (e.target.value = ""))} />
          <Dictation onText={(t) => setText((cur) => (cur && !/\s$/.test(cur) ? `${cur} ${t}` : cur + t))} target={ref} />
          {config?.agent.kind && <><ChatModelPicker chat={chat} openSignal={picker.which === "model" ? picker.n : 0} /><ModePicker chat={chat} openSignal={picker.which === "mode" ? picker.n : 0} /></>}
          {extra}
          {chat?.messages && <ContextMeter messages={chat.messages} />}
        </span>
        {running && chat?.session && (
          <Button size="sm" icon="stop" className="composer-stop" title="Stop the agent" onClick={() => api.chatStop(chat.session!.id)}>Stop</Button>
        )}
        <Button size="sm" variant="primary" icon={shellMode ? "terminal" : "arrow-up"} disabled={(!text.trim() && !images.length) || uploading > 0} onClick={submit}>{shellMode ? "Run" : running ? "Steer" : "Send"}</Button>
      </div>
    </div>
  );
}
