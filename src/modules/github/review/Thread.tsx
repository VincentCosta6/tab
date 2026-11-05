// Review threads, the comment composer (a GitHub-style markdown editor with
// suggestions), and comment bodies with rendered suggested changes.

import { useEffect, useRef, useState, type ReactNode } from "react";
import { Badge, Button, cx } from "../../../design/components";
import { Icon } from "../../../design/icons";
import { Markdown } from "../../../design/markdown";
import type { ReviewThread, ThreadComment } from "../../../lib/types";
import { headUri, baseUri, monaco } from "../monaco";
import { useReview, type Side } from "./context";
import { threadRange, timeAgo } from "./model";

export function Avatar({ login, url, size = 20 }: { login: string; url?: string | null; size?: number }) {
  const [broken, setBroken] = useState(false);
  if (url && !broken) return <img className="rv-avatar" src={url} width={size} height={size} alt="" onError={() => setBroken(true)} />;
  const hue = [...login].reduce((h, c) => (h * 31 + c.charCodeAt(0)) % 360, 7);
  return (
    <span className="rv-avatar rv-avatar-initials" style={{ width: size, height: size, fontSize: size * 0.45, background: `hsl(${hue} 55% 45%)` }}>
      {login.slice(0, 1).toUpperCase()}
    </span>
  );
}

/** The lines a comment is about, read from the open model. */
function linesOf(path: string, side: Side, start: number, end: number): string[] | null {
  const m = monaco.editor.getModel(side === "LEFT" ? baseUri(path) : headUri(path));
  if (!m) return null;
  const out: string[] = [];
  for (let i = start; i <= Math.min(end, m.getLineCount()); i++) out.push(m.getLineContent(i));
  return out;
}

/** A comment body: markdown, with ```suggestion blocks shown as a change. */
export function RichBody({ body, path, side, range }: { body: string; path?: string; side?: Side; range?: [number, number] | null }) {
  const parts: ReactNode[] = [];
  const re = /```(\w*)[^\n]*\n([\s\S]*?)(?:\n)?```/g;
  let last = 0;
  let m: RegExpExecArray | null;
  let i = 0;
  while ((m = re.exec(body))) {
    const text = body.slice(last, m.index).trim();
    if (text) parts.push(<Markdown key={i++} text={text} />);
    if (m[1] === "suggestion") {
      const old = path && side && range ? linesOf(path, side, range[0], range[1]) : null;
      const next = m[2] === "" ? [] : m[2].split("\n");
      parts.push(
        <div key={i++} className="rv-suggestion">
          <div className="rv-suggestion-head"><Icon name="file-diff" size={13} /> Suggested change</div>
          <pre>
            {old?.map((l, j) => <div key={`o${j}`} className="rv-del">{l || " "}</div>)}
            {next.map((l, j) => <div key={`n${j}`} className="rv-add">{l || " "}</div>)}
          </pre>
        </div>,
      );
    } else parts.push(<pre key={i++} className="rv-code"><code>{m[2]}</code></pre>);
    last = m.index + m[0].length;
  }
  const rest = body.slice(last).trim();
  if (rest) parts.push(<Markdown key={i++} text={rest} />);
  if (!parts.length) parts.push(<p key="empty" className="subtle">No description.</p>);
  return <div className="rv-richbody">{parts}</div>;
}

export interface ComposerButton { label: string; kind: string; primary?: boolean }

const TOOLS: { icon: string; title: string; kind: "wrap" | "line"; a: string; b?: string; key?: string }[] = [
  { icon: "bold", title: "Bold (Ctrl B)", kind: "wrap", a: "**", b: "**", key: "b" },
  { icon: "italic", title: "Italic (Ctrl I)", kind: "wrap", a: "_", b: "_", key: "i" },
  { icon: "quote", title: "Quote", kind: "line", a: "> " },
  { icon: "code", title: "Code (Ctrl E)", kind: "wrap", a: "`", b: "`", key: "e" },
  { icon: "link", title: "Link (Ctrl K)", kind: "wrap", a: "[", b: "](url)", key: "k" },
  { icon: "list", title: "Bulleted list", kind: "line", a: "- " },
  { icon: "list-ordered", title: "Numbered list", kind: "line", a: "1. " },
  { icon: "check-square", title: "Task list", kind: "line", a: "- [ ] " },
];

/** Replaces the selection in a textarea, keeping the browser's undo history. */
function replaceSelection(ta: HTMLTextAreaElement, text: string, selectFrom?: number, selectTo?: number) {
  const start = ta.selectionStart;
  ta.focus();
  if (!document.execCommand("insertText", false, text)) {
    ta.setRangeText(text, ta.selectionStart, ta.selectionEnd, "end");
    ta.dispatchEvent(new Event("input", { bubbles: true }));
  }
  if (selectFrom != null) ta.setSelectionRange(start + selectFrom, start + (selectTo ?? selectFrom));
}

export function Composer({
  initial = "", placeholder = "Leave a comment", buttons, onSubmit, onCancel, target, autoFocus = true, minRows = 4, hideButtons, onChange,
}: {
  initial?: string;
  placeholder?: string;
  buttons: ComposerButton[];
  onSubmit?: (body: string, kind: string) => Promise<void> | void;
  onCancel?: () => void;
  /** The lines being commented on; enables suggestions. */
  target?: { path: string; side: Side; start: number; end: number };
  autoFocus?: boolean;
  minRows?: number;
  hideButtons?: boolean;
  onChange?: (body: string) => void;
}) {
  const [text, setText] = useState(initial);
  const [tab, setTab] = useState<"write" | "preview">("write");
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const ta = useRef<HTMLTextAreaElement>(null);

  useEffect(() => {
    if (autoFocus) ta.current?.focus();
  }, [autoFocus]);
  useEffect(() => onChange?.(text), [text, onChange]);

  const tool = (t: (typeof TOOLS)[number]) => {
    const el = ta.current;
    if (!el) return;
    const sel = el.value.slice(el.selectionStart, el.selectionEnd);
    if (t.kind === "wrap") {
      const inner = sel || (t.icon === "link" ? "text" : "");
      replaceSelection(el, `${t.a}${inner}${t.b}`, t.a.length, t.a.length + inner.length);
    } else {
      const lineStart = el.value.lastIndexOf("\n", el.selectionStart - 1) + 1;
      el.setSelectionRange(lineStart, el.selectionEnd);
      const lines = el.value.slice(lineStart, el.selectionEnd).split("\n");
      replaceSelection(el, lines.map((l, i) => (t.a === "1. " ? `${i + 1}. ` : t.a) + l).join("\n"));
    }
  };

  const suggest = () => {
    const el = ta.current;
    if (!el || !target) return;
    const lines = linesOf(target.path, target.side, target.start, target.end) ?? [];
    const prefix = el.selectionStart > 0 && el.value[el.selectionStart - 1] !== "\n" ? "\n" : "";
    replaceSelection(el, `${prefix}\`\`\`suggestion\n${lines.join("\n")}\n\`\`\`\n`);
  };

  const submit = async (kind: string) => {
    if (!onSubmit || !text.trim() || busy) return;
    setBusy(kind);
    setError(null);
    try {
      await onSubmit(text.trim(), kind);
      setText("");
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
    }
  };

  const onKey = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    const mod = e.metaKey || e.ctrlKey;
    if (mod && e.key === "Enter") {
      e.preventDefault();
      const b = buttons.find((b) => b.primary) ?? buttons[0];
      if (b) submit(b.kind);
    } else if (e.key === "Escape" && onCancel) {
      e.preventDefault();
      onCancel();
    } else if (mod && !e.shiftKey) {
      const t = TOOLS.find((t) => t.key === e.key.toLowerCase());
      if (t) {
        e.preventDefault();
        tool(t);
      }
    }
  };

  return (
    <div className="rv-composer">
      <div className="rv-composer-bar">
        <div className="rv-composer-tabs">
          <button className={cx(tab === "write" && "active")} onClick={() => setTab("write")}>Write</button>
          <button className={cx(tab === "preview" && "active")} onClick={() => setTab("preview")}>Preview</button>
        </div>
        {tab === "write" && (
          <div className="rv-composer-tools">
            {target && (
              <button className="rv-tool rv-tool-suggest" title="Suggest a change to these lines" onClick={suggest}>
                <Icon name="file-diff" size={14} /> <span>Suggest</span>
              </button>
            )}
            {TOOLS.map((t) => (
              <button key={t.icon} className="rv-tool" title={t.title} onClick={() => tool(t)}>
                <Icon name={t.icon} size={14} />
              </button>
            ))}
          </div>
        )}
      </div>
      {tab === "write" ? (
        <textarea
          ref={ta}
          className="rv-textarea"
          rows={minRows}
          value={text}
          placeholder={placeholder}
          onChange={(e) => setText(e.target.value)}
          onKeyDown={onKey}
        />
      ) : (
        <div className="rv-preview">{text.trim() ? <RichBody body={text} path={target?.path} side={target?.side} range={target ? [target.start, target.end] : null} /> : <p className="subtle">Nothing to preview.</p>}</div>
      )}
      {error && <p className="rv-error">{error}</p>}
      {!hideButtons && (
        <div className="rv-composer-actions">
          <span className="subtle rv-hint">Markdown supported · Ctrl Enter to send</span>
          {onCancel && <Button size="sm" variant="ghost" onClick={onCancel}>Cancel</Button>}
          {buttons.map((b) => (
            <Button key={b.kind} size="sm" variant={b.primary ? "primary" : "secondary"} disabled={!text.trim() || !!busy} onClick={() => submit(b.kind)}>
              {busy === b.kind ? "Sending…" : b.label}
            </Button>
          ))}
        </div>
      )}
    </div>
  );
}

function CommentView({ thread, c, range }: { thread: ReviewThread; c: ThreadComment; range: [number, number] | null }) {
  const { act, notify } = useReview();
  const [editing, setEditing] = useState(false);
  const [menu, setMenu] = useState(false);
  return (
    <div className={cx("rv-comment", c.pending && "pending")}>
      <Avatar login={c.author} url={c.avatar} size={22} />
      <div className="rv-comment-main">
        <div className="rv-comment-head">
          <strong>{c.author}</strong>
          <span className="subtle">{timeAgo(c.created_at)}</span>
          {c.pending && <Badge tone="warning">Pending</Badge>}
          <span className="rv-flex" />
          {c.viewer_did_author && (
            <span className="rv-menu-wrap">
              <button className="rv-icon-btn" title="More" onClick={() => setMenu((m) => !m)}><Icon name="more" size={14} /></button>
              {menu && (
                <div className="rv-menu" onMouseLeave={() => setMenu(false)}>
                  <button onClick={() => { setMenu(false); setEditing(true); }}><Icon name="edit" size={13} /> Edit</button>
                  <button className="text-danger" onClick={() => { setMenu(false); act({ type: "delete_comment", comment_id: c.id }).catch((e) => notify(String(e), "danger")); }}><Icon name="trash" size={13} /> Delete</button>
                </div>
              )}
            </span>
          )}
        </div>
        {editing ? (
          <Composer
            initial={c.body}
            buttons={[{ label: "Update comment", kind: "save", primary: true }]}
            target={range ? { path: thread.path, side: thread.side, start: range[0], end: range[1] } : undefined}
            onCancel={() => setEditing(false)}
            onSubmit={async (body) => { await act({ type: "edit_comment", comment_id: c.id, body }); setEditing(false); }}
          />
        ) : (
          <RichBody body={c.body} path={thread.path} side={thread.side} range={range} />
        )}
      </div>
    </div>
  );
}

export function ThreadCard({ thread }: { thread: ReviewThread }) {
  const { pr, act, notify } = useReview();
  const [open, setOpen] = useState(!thread.resolved);
  const [replying, setReplying] = useState(false);
  const range = threadRange(thread);
  const hasPending = !!pr.pending_review;
  const where = range ? (range[0] === range[1] ? `Line ${range[1]}` : `Lines ${range[0]}–${range[1]}`) : thread.original_line ? `Line ${thread.original_line}` : "File";
  const resolve = (resolved: boolean) => act({ type: "resolve", thread_id: thread.id, resolved }).catch((e) => notify(String(e), "danger"));
  const onlyPending = thread.comments.every((c) => c.pending);

  return (
    <div className={cx("rv-thread", thread.resolved && "resolved", onlyPending && "pending")}>
      <button className="rv-thread-head" onClick={() => setOpen((o) => !o)}>
        <Icon name={open ? "chevron-down" : "chevron-right"} size={13} />
        <span>{where}{thread.side === "LEFT" ? " (base)" : ""}</span>
        {thread.outdated && <Badge tone="warning">Outdated</Badge>}
        {thread.resolved && <Badge tone="success" icon="check">Resolved</Badge>}
        {!open && <span className="subtle rv-thread-peek">{thread.comments[0]?.author}: {thread.comments[0]?.body.slice(0, 80)}</span>}
        <span className="rv-flex" />
        <span className="subtle">{thread.comments.length} comment{thread.comments.length === 1 ? "" : "s"}</span>
      </button>
      {open && (
        <>
          <div className="rv-thread-comments">
            {thread.comments.map((c) => <CommentView key={c.id} thread={thread} c={c} range={range} />)}
          </div>
          <div className="rv-thread-foot">
            {replying ? (
              <Composer
                placeholder="Reply…"
                minRows={3}
                target={range ? { path: thread.path, side: thread.side, start: range[0], end: range[1] } : undefined}
                buttons={hasPending || onlyPending ? [{ label: "Add to review", kind: "review", primary: true }] : [{ label: "Reply now", kind: "single" }, { label: "Add to review", kind: "review", primary: true }]}
                onCancel={() => setReplying(false)}
                onSubmit={async (body, kind) => {
                  const first = thread.comments.find((c) => !c.pending && c.database_id);
                  await act({ type: "reply", thread_id: thread.id, comment_id: first?.database_id ?? null, body, pending: kind === "review" });
                  setReplying(false);
                }}
              />
            ) : (
              <div className="rv-thread-actions">
                {thread.can_reply && <button className="rv-reply-fake" onClick={() => setReplying(true)}>Reply…</button>}
                {thread.can_resolve && !onlyPending && (
                  <Button size="sm" icon={thread.resolved ? "undo" : "check"} onClick={() => { resolve(!thread.resolved); if (!thread.resolved) setOpen(false); }}>
                    {thread.resolved ? "Unresolve" : "Resolve"}
                  </Button>
                )}
              </div>
            )}
          </div>
        </>
      )}
    </div>
  );
}

/** The composer for a new thread on a line range. */
export function NewThread({ path, side, start, end, onDone }: { path: string; side: Side; start: number; end: number; onDone: () => void }) {
  const { pr, act } = useReview();
  const hasPending = !!pr.pending_review;
  return (
    <div className="rv-thread rv-thread-new">
      <div className="rv-thread-head static">
        <Icon name="message" size={13} />
        <span>{start === end ? `Comment on line ${end}` : `Comment on lines ${start}–${end}`}{side === "LEFT" ? " (base)" : ""}</span>
      </div>
      <div className="rv-thread-foot">
        <Composer
          target={{ path, side, start, end }}
          buttons={[
            { label: "Add single comment", kind: "single" },
            { label: hasPending ? "Add review comment" : "Start a review", kind: "review", primary: true },
          ]}
          onCancel={onDone}
          onSubmit={async (body, kind) => {
            await act({ type: "add_thread", path, line: end, start_line: start < end ? start : null, side, body, pending: kind === "review" });
            onDone();
          }}
        />
      </div>
    </div>
  );
}
