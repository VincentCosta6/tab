// The review workspace's AI: one agent chat per PR (side panel), Ask AI on
// lines of code, and a fix list of things for the agent to change, run from
// a banner under the header. The agent runs in the PR's checkout; its replies
// link code as `path#L12`, which open in the workspace (or VS Code).

import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useCallback, useEffect, useMemo, useState, type MouseEvent } from "react";
import { Button, cx } from "../../../design/components";
import { Icon } from "../../../design/icons";
import { MarkdownLinks } from "../../../design/markdown";
import { api } from "../../../lib/api";
import { keys } from "../../../lib/query";
import type { Chat, PullReview } from "../../../lib/types";
import { Composer as ChatComposer, Transcript } from "../../work/Chat";
import { baseUri, headUri, languageFor, monaco } from "../monaco";
import type { Side } from "./context";

export interface CodeRef { path: string; side: Side; start: number; end: number }
export interface FixItem {
  id: string;
  /** What to change, in the person's (or the reviewer's) words. */
  text: string;
  at: CodeRef | null;
  /** The code it's about, as it was when added. */
  code: string | null;
  /** Where it came from: a new note or a review thread. */
  from: "note" | "thread";
  state: "queued" | "sent";
  added_at: string;
}

export interface Ai {
  open: boolean;
  setOpen: (o: boolean) => void;
  /** Asks the agent about some code (or the PR) and opens the panel. */
  ask: (question: string, at?: CodeRef | null) => Promise<void>;
  fixes: FixItem[];
  addFix: (text: string, at: CodeRef | null, from?: FixItem["from"]) => void;
  removeFix: (id: string) => void;
  runFixes: () => Promise<void>;
  clearSent: () => void;
  chat: Chat | undefined;
}

const chatKey = (pr: PullReview) => keys.chat(`review:${pr.repo}#${pr.number}`);
const fixKey = (pr: PullReview) => `tab.review.fixes.${pr.repo}#${pr.number}`;

function loadFixes(pr: PullReview): FixItem[] {
  try {
    return JSON.parse(localStorage.getItem(fixKey(pr)) ?? "[]");
  } catch {
    return [];
  }
}

/** The lines a ref covers, read from the open model. */
export function codeAt(at: CodeRef): string | null {
  const m = monaco.editor.getModel(at.side === "LEFT" ? baseUri(at.path) : headUri(at.path));
  if (!m) return null;
  const out: string[] = [];
  for (let i = at.start; i <= Math.min(at.end, m.getLineCount()); i++) out.push(m.getLineContent(i));
  return out.join("\n");
}

const anchor = (at: CodeRef) => `${at.path}#L${at.start}${at.end > at.start ? `-L${at.end}` : ""}`;
const lines = (at: CodeRef) => (at.start === at.end ? `line ${at.end}` : `lines ${at.start}–${at.end}`);
const fence = (at: CodeRef, code: string) => {
  const lang = languageFor(at.path);
  const ticks = code.includes("```") ? "````" : "```";
  return `${ticks}${lang === "plaintext" ? "" : lang}\n${code}\n${ticks}`;
};

export function describeRef(at: CodeRef) {
  return `${at.path} ${lines(at)}${at.side === "LEFT" ? " (before the change)" : ""}`;
}

export function useAi(pr: PullReview | undefined, notify: (t: string, tone?: "danger" | "success") => void): Ai | null {
  const qc = useQueryClient();
  const [open, setOpen] = useState(false);
  const [fixes, setFixes] = useState<FixItem[]>([]);
  const prKey = pr ? `${pr.repo}#${pr.number}` : "";

  useEffect(() => {
    if (pr) setFixes(loadFixes(pr));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [prKey]);

  const save = useCallback((f: (xs: FixItem[]) => FixItem[]) => {
    if (!pr) return;
    setFixes((xs) => {
      const next = f(xs);
      try {
        localStorage.setItem(fixKey(pr), JSON.stringify(next));
      } catch {
        // Kept for this session only.
      }
      return next;
    });
  }, [pr]);

  const chat = useQuery<Chat>({
    queryKey: pr ? chatKey(pr) : ["chat", ""],
    queryFn: () => api.reviewChatOpen(pr!.repo, pr!.number),
    enabled: !!pr,
    staleTime: 0,
    refetchOnMount: "always",
  }).data;

  const send = useCallback(async (text: string) => {
    if (!pr) return;
    const key = chatKey(pr);
    // Show the message at once; the stored copy replaces it.
    qc.setQueryData<Chat>(key, (c) => ({
      session: c?.session ?? null,
      approvals: c?.approvals ?? [],
      running: true,
      messages: [...(c?.messages ?? []), { id: -Date.now(), session_id: c?.session?.id ?? "", role: "user", kind: "text", content: text, meta: null, created_at: new Date().toISOString() }],
    }));
    try {
      const session = await api.reviewChatSend({ repo: pr.repo, number: pr.number, title: pr.title, url: pr.url, head_ref: pr.head_ref, base_ref: pr.base_ref }, text);
      qc.setQueryData<Chat>(key, (c) => ({ session, approvals: c?.approvals ?? [], running: true, messages: (c?.messages ?? []).filter((m) => m.id > 0) }));
      await qc.invalidateQueries({ queryKey: key });
    } catch (e) {
      await qc.invalidateQueries({ queryKey: key });
      throw e;
    }
  }, [pr, qc]);

  return useMemo<Ai | null>(() => {
    if (!pr) return null;
    return {
      open,
      setOpen,
      chat,
      fixes,
      ask: async (question, at) => {
        setOpen(true);
        const q = question.trim() || (at ? "What does this code do?" : "Summarize this pull request.");
        const code = at && codeAt(at);
        const text = at ? `About [${describeRef(at)}](${anchor(at)}):\n\n${code ? `${fence(at, code)}\n\n` : ""}${q}` : q;
        await send(text).catch((e) => notify(String(e), "danger"));
      },
      addFix: (text, at, from = "note") => {
        save((xs) => [...xs, { id: `fix-${Date.now()}-${xs.length}`, text: text.trim(), at, code: at ? codeAt(at) : null, from, state: "queued", added_at: new Date().toISOString() }]);
        notify("Added to the fix list", "success");
      },
      removeFix: (id) => save((xs) => xs.filter((x) => x.id !== id)),
      clearSent: () => save((xs) => xs.filter((x) => x.state !== "sent")),
      runFixes: async () => {
        const queued = fixes.filter((f) => f.state === "queued");
        if (!queued.length) return;
        const body = queued
          .map((f, i) => {
            const where = f.at ? `[${describeRef(f.at)}](${anchor(f.at)})` : "General";
            const code = f.at && f.code ? `\n\n${fence(f.at, f.code)}` : "";
            return `${i + 1}. ${where}: ${f.text.replace(/\n/g, "\n   ")}${code.replace(/\n/g, "\n   ")}`;
          })
          .join("\n\n");
        setOpen(true);
        await send(`Here's my fix list for this pull request. Make each fix in the checkout, then tell me what you changed for each one.\n\n${body}`);
        save((xs) => xs.map((x) => (queued.some((q) => q.id === x.id) ? { ...x, state: "sent" } : x)));
      },
    };
  }, [pr, open, chat, fixes, send, save, notify]);
}

/** `src/a.ts#L12-L20` → path and line. */
export function parseCodeLink(href: string): { path: string; line?: number } | null {
  if (/^[a-z]+:/i.test(href) || href.startsWith("#")) return null;
  const [raw, frag = ""] = href.split("#");
  const path = decodeURIComponent(raw.replace(/^\.?\//, ""));
  if (!path) return null;
  const m = /^L(\d+)/.exec(frag);
  return { path, line: m ? Number(m[1]) : undefined };
}

/** Under the header while the fix list has anything in it. */
export function FixBanner({ ai, local }: { ai: Ai; local: boolean }) {
  const [list, setList] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const queued = ai.fixes.filter((f) => f.state === "queued");
  const sent = ai.fixes.filter((f) => f.state === "sent");
  if (!ai.fixes.length) return <div />;
  const running = !!ai.chat?.running;
  return (
    <div className="rv-fixbar">
      <div className="rv-fixbar-row">
        <Icon name="zap" size={14} />
        <strong>Fix list</strong>
        <span className="muted">
          {queued.length ? `${queued.length} fix${queued.length === 1 ? "" : "es"} for your agent` : "Nothing queued"}
          {sent.length ? ` · ${sent.length} sent` : ""}
          {!local && queued.length ? " · no local checkout, so the agent works from GitHub" : ""}
        </span>
        <button className="rv-link" onClick={() => setList((l) => !l)}>{list ? "Hide list" : "Show list"}</button>
        <span className="rv-flex" />
        {error && <span className="text-danger rv-fixbar-err" title={error}>{error}</span>}
        {sent.length > 0 && <Button size="sm" variant="ghost" onClick={ai.clearSent}>Clear sent</Button>}
        {sent.length > 0 && <Button size="sm" variant="ghost" icon="message" onClick={() => ai.setOpen(true)}>View chat</Button>}
        <Button
          size="sm"
          variant="primary"
          icon="sparkles"
          disabled={!queued.length || busy || running}
          title={running ? "The agent is still working" : undefined}
          onClick={async () => {
            setBusy(true);
            setError(null);
            try {
              await ai.runFixes();
            } catch (e) {
              setError(String(e));
            } finally {
              setBusy(false);
            }
          }}
        >
          {busy ? "Sending…" : `Run fix list${queued.length ? ` (${queued.length})` : ""}`}
        </Button>
      </div>
      {list && (
        <ol className="rv-fixlist">
          {ai.fixes.map((f) => (
            <li key={f.id} className={cx(f.state === "sent" && "sent")}>
              <div className="rv-fix-text">
                {f.at && <span className="rv-mono subtle">{describeRef(f.at)}</span>}
                <span>{f.text}</span>
              </div>
              {f.state === "sent" ? <span className="subtle">Sent</span> : null}
              <button className="rv-icon-btn" title="Remove" onClick={() => ai.removeFix(f.id)}><Icon name="x" size={12} /></button>
            </li>
          ))}
        </ol>
      )}
    </div>
  );
}

/** The PR's agent chat, beside the editor. */
export function AiPanel({ ai, pr, openFile }: { ai: Ai; pr: PullReview; openFile: (path: string, line?: number) => void }) {
  const [vscodeLinks, setVscodeLinks] = useState(() => {
    try {
      return localStorage.getItem("tab.review.aiLinks") === "vscode";
    } catch {
      return false;
    }
  });
  const [error, setError] = useState<string | null>(null);
  const onLink = useCallback((href: string, e: MouseEvent) => {
    const l = parseCodeLink(href);
    if (!l) return;
    const external = vscodeLinks !== (e.metaKey || e.ctrlKey);
    if (external && !pr.sample) api.workspaceOpen(pr.repo, pr.number, "vscode", l.path, l.line).catch((err) => setError(String(err)));
    else openFile(l.path, l.line);
  }, [vscodeLinks, pr, openFile]);
  const setLinks = (v: boolean) => {
    setVscodeLinks(v);
    try {
      localStorage.setItem("tab.review.aiLinks", v ? "vscode" : "tab");
    } catch {
      // Per session only.
    }
  };
  const send = async (text: string) => {
    setError(null);
    await ai.ask(text);
  };
  return (
    <aside className="rv-ai">
      <div className="rv-ai-head">
        <Icon name="sparkles" size={14} />
        <strong>Ask AI</strong>
        <span className="subtle">about this PR</span>
        <span className="rv-flex" />
        <div className="rv-seg" title="Where code links in replies open (Ctrl or Cmd click for the other)">
          <button className={cx(!vscodeLinks && "on")} onClick={() => setLinks(false)}>tab</button>
          <button className={cx(vscodeLinks && "on")} onClick={() => setLinks(true)} disabled={pr.sample}>VS Code</button>
        </div>
        <button className="rv-icon-btn" title="Close" onClick={() => ai.setOpen(false)}><Icon name="x" size={14} /></button>
      </div>
      <MarkdownLinks.Provider value={onLink}>
        <div className="rv-ai-body chat">
          <Transcript
            chat={ai.chat}
            suggestions={["Summarize this PR and its risks", "What could break with this change?", "Which tests cover this change?", "Explain the riskiest file"]}
            onSuggest={send}
            onSend={send}
            intro={<p className="muted rv-ai-intro">The agent works in this PR's checkout. Its replies link the code they mention; click a link to jump there.</p>}
          />
          {error && <div className="chat-error"><Icon name="x-circle" size={14} /> {error}</div>}
          <ChatComposer chat={ai.chat} onSend={send} placeholder="Ask about this PR, or tell the agent what to change…" />
        </div>
      </MarkdownLinks.Provider>
    </aside>
  );
}
