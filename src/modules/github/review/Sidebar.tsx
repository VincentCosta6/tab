// The left side: an activity bar (Explorer, Search, Changes, Threads) and
// the panel for whichever is active.

import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { Meter, cx } from "../../../design/components";
import { Icon } from "../../../design/icons";
import type { PrFile, SearchQuery } from "../../../lib/types";
import { useReview } from "./context";
import { useSearch } from "./data";
import { buildTree, isViewed, splitPath, statusLetter, statusTone, type TreeNode } from "./model";

export type Panel = "explorer" | "search" | "changes" | "threads";

export function ActivityBar({ panel, onPanel, unread, threads }: { panel: Panel | null; onPanel: (p: Panel) => void; unread: number; threads: number }) {
  const item = (p: Panel, icon: string, label: string, badge?: number) => (
    <button key={p} className={cx("rv-activity-item", panel === p && "active")} onClick={() => onPanel(p)} title={label}>
      <Icon name={icon} size={20} />
      {!!badge && <span className="rv-activity-badge">{badge > 99 ? "99+" : badge}</span>}
    </button>
  );
  return (
    <nav className="rv-activity">
      {item("explorer", "files", "Explorer (Ctrl Shift E)")}
      {item("search", "search", "Search (Ctrl Shift F)")}
      {item("changes", "file-diff", "Files changed (Ctrl Shift G)", unread)}
      {item("threads", "message", "Review threads", threads)}
    </nav>
  );
}

function PanelHead({ title, children }: { title: string; children?: ReactNode }) {
  return (
    <div className="rv-panel-head">
      <span className="label-sm">{title}</span>
      <span className="rv-flex" />
      {children}
    </div>
  );
}

// --- Explorer --------------------------------------------------------------

function TreeRow({ node, depth, changed, open, toggle, active, onOpen, dirtyDirs }: {
  node: TreeNode; depth: number; changed: Map<string, PrFile>; open: Set<string>; toggle: (p: string) => void;
  active: string | null; onOpen: (p: string) => void; dirtyDirs: Set<string>;
}) {
  const f = changed.get(node.path);
  const isOpen = open.has(node.path);
  return (
    <>
      <button
        className={cx("rv-tree-row", active === node.path && "active", f && `changed tone-${statusTone[f.status] ?? "warning"}`)}
        style={{ paddingLeft: 8 + depth * 12 }}
        onClick={() => (node.dir ? toggle(node.path) : onOpen(node.path))}
        title={node.path}
      >
        {node.dir ? <Icon name={isOpen ? "chevron-down" : "chevron-right"} size={13} /> : <span className="rv-tree-spacer" />}
        <Icon name={node.dir ? (isOpen ? "folder-open" : "folder") : "file"} size={14} className={node.dir ? "rv-folder-icon" : "rv-file-icon"} />
        <span className="rv-tree-name">{node.name}</span>
        {f && <span className="rv-tree-status">{statusLetter[f.status] ?? "M"}</span>}
        {!f && node.dir && dirtyDirs.has(node.path) && <span className="rv-tree-dot" />}
      </button>
      {node.dir && isOpen && node.children.map((c) => (
        <TreeRow key={c.path} node={c} depth={depth + 1} changed={changed} open={open} toggle={toggle} active={active} onOpen={onOpen} dirtyDirs={dirtyDirs} />
      ))}
    </>
  );
}

export function Explorer({ files, active }: { files: string[]; active: string | null }) {
  const { pr, openFile } = useReview();
  const [filter, setFilter] = useState("");
  const changed = useMemo(() => new Map(pr.files.map((f) => [f.filename, f])), [pr.files]);
  const dirtyDirs = useMemo(() => {
    const s = new Set<string>();
    for (const f of pr.files) {
      const parts = f.filename.split("/");
      for (let i = 1; i < parts.length; i++) s.add(parts.slice(0, i).join("/"));
    }
    return s;
  }, [pr.files]);
  // Open the folders that hold changes, so the PR's files are one click away.
  const [open, setOpen] = useState<Set<string>>(() => new Set(dirtyDirs));
  const shown = useMemo(() => {
    const q = filter.trim().toLowerCase();
    return q ? files.filter((f) => f.toLowerCase().includes(q)) : files;
  }, [files, filter]);
  const tree = useMemo(() => buildTree(shown), [shown]);
  const openAll = useMemo(() => (filter ? new Set(shown.flatMap((f) => f.split("/").slice(0, -1).map((_, i, a) => a.slice(0, i + 1).join("/")))) : open), [filter, shown, open]);
  const toggle = (p: string) => setOpen((s) => {
    const n = new Set(s);
    if (n.has(p)) n.delete(p);
    else n.add(p);
    return n;
  });
  return (
    <div className="rv-panel">
      <PanelHead title={pr.repo.split("/")[1] ?? pr.repo}>
        <button className="rv-icon-btn" title="Collapse folders" onClick={() => setOpen(new Set())}><Icon name="chevrons-up" size={14} /></button>
      </PanelHead>
      <div className="rv-panel-filter">
        <input className="rv-input" placeholder="Filter files" value={filter} onChange={(e) => setFilter(e.target.value)} />
      </div>
      <div className="rv-tree">
        {tree.map((n) => <TreeRow key={n.path} node={n} depth={0} changed={changed} open={openAll} toggle={toggle} active={active} onOpen={(p) => openFile(p)} dirtyDirs={dirtyDirs} />)}
        {!tree.length && <p className="subtle rv-pad">No files match.</p>}
      </div>
    </div>
  );
}

// --- Search ----------------------------------------------------------------

function Highlight({ text, column, length }: { text: string; column: number; length: number }) {
  const start = Math.max(0, column - 1);
  const lead = Math.max(0, start - 30);
  return (
    <span className="rv-hit-text">
      {lead > 0 && "…"}
      {text.slice(lead, start).trimStart()}
      <mark>{text.slice(start, start + length)}</mark>
      {text.slice(start + length, start + length + 120)}
    </span>
  );
}

export function SearchPanel({ seed, local }: { seed: { text: string; nonce: number } | null; local: boolean }) {
  const { src, openFile } = useReview();
  const [q, setQ] = useState<SearchQuery>({ query: "", regex: false, case_sensitive: false, whole_word: false, include: null });
  const [typed, setTyped] = useState("");
  const [showInclude, setShowInclude] = useState(false);
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());
  const input = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (seed) {
      setTyped(seed.text);
      setQ((q) => ({ ...q, query: seed.text }));
    }
    input.current?.focus();
    input.current?.select();
  }, [seed]);
  useEffect(() => {
    const t = window.setTimeout(() => setQ((q) => (q.query === typed ? q : { ...q, query: typed })), 220);
    return () => clearTimeout(t);
  }, [typed]);

  const { data: hits = [], isFetching, error } = useSearch(src, q);
  const groups = useMemo(() => {
    const m = new Map<string, typeof hits>();
    for (const h of hits) m.set(h.path, [...(m.get(h.path) ?? []), h]);
    return [...m];
  }, [hits]);
  const matchLen = (text: string, column: number) => {
    if (!q.regex) return q.query.length;
    try {
      const m = new RegExp(q.query, q.case_sensitive ? "" : "i").exec(text.slice(column - 1));
      return m?.[0].length || q.query.length;
    } catch {
      return q.query.length;
    }
  };
  const toggle = (k: keyof SearchQuery) => setQ((x) => ({ ...x, [k]: !x[k] }));

  return (
    <div className="rv-panel">
      <PanelHead title="Search" />
      <div className="rv-search-box">
        <div className="rv-search-input">
          <input ref={input} className="rv-input" placeholder="Search the repository" value={typed} onChange={(e) => setTyped(e.target.value)} />
          <button className={cx("rv-toggle", q.case_sensitive && "on")} title="Match case" onClick={() => toggle("case_sensitive")}>Aa</button>
          <button className={cx("rv-toggle", q.whole_word && "on")} title="Match whole word" onClick={() => toggle("whole_word")}><u>ab</u></button>
          <button className={cx("rv-toggle", q.regex && "on")} title="Use regular expression" onClick={() => toggle("regex")}>.*</button>
        </div>
        <button className="rv-link" onClick={() => setShowInclude((s) => !s)}>{showInclude ? "Hide filters" : "Files to include…"}</button>
        {showInclude && <input className="rv-input" placeholder="e.g. src/**/*.ts, *.rs" value={q.include ?? ""} onChange={(e) => setQ((x) => ({ ...x, include: e.target.value || null }))} />}
      </div>
      <div className="rv-search-meta subtle">
        {!local ? "Search needs the local copy of this repository." : error ? <span className="text-danger">{String(error)}</span> : q.query ? (isFetching ? "Searching…" : `${hits.length}${hits.length >= 2000 ? "+" : ""} results in ${groups.length} files`) : "Searches every file at the PR's head."}
      </div>
      <div className="rv-search-results">
        {groups.map(([path, hs]) => {
          const { dir, name } = splitPath(path);
          const isCollapsed = collapsed.has(path);
          return (
            <div key={path}>
              <button className="rv-hit-file" onClick={() => setCollapsed((s) => { const n = new Set(s); if (n.has(path)) n.delete(path); else n.add(path); return n; })}>
                <Icon name={isCollapsed ? "chevron-right" : "chevron-down"} size={13} />
                <Icon name="file" size={13} />
                <strong>{name}</strong>
                <span className="subtle rv-ellipsis">{dir}</span>
                <span className="rv-flex" />
                <span className="rv-count">{hs.length}</span>
              </button>
              {!isCollapsed && hs.map((h) => (
                <button key={`${h.line}:${h.column}`} className="rv-hit" onClick={() => openFile(h.path, h.line, h.column)}>
                  <span className="rv-hit-line">{h.line}</span>
                  <Highlight text={h.text} column={h.column} length={matchLen(h.text, h.column)} />
                </button>
              ))}
            </div>
          );
        })}
      </div>
    </div>
  );
}

// --- Changes ---------------------------------------------------------------

export function ChangesPanel({ autoRead, setAutoRead, active }: { autoRead: boolean; setAutoRead: (v: boolean) => void; active: string | null }) {
  const { pr, act, showChange, notify } = useReview();
  const [filter, setFilter] = useState("");
  const [hideViewed, setHideViewed] = useState(false);
  const viewed = pr.files.filter(isViewed).length;
  const shown = pr.files.filter((f) => (!filter || f.filename.toLowerCase().includes(filter.toLowerCase())) && (!hideViewed || !isViewed(f)));
  return (
    <div className="rv-panel">
      <PanelHead title="Files changed">
        <span className="subtle rv-tabular">{pr.files.length}</span>
      </PanelHead>
      <div className="rv-changes-progress">
        <div className="rv-changes-progress-row">
          <span><strong>{viewed}</strong> of {pr.files.length} viewed</span>
          <span className="subtle rv-tabular"><span className="text-success">+{pr.additions}</span> <span className="text-danger">−{pr.deletions}</span></span>
        </div>
        <Meter value={viewed} max={Math.max(1, pr.files.length)} tone={viewed === pr.files.length ? "success" : "accent"} />
        <label className="rv-check">
          <input type="checkbox" checked={autoRead} onChange={(e) => setAutoRead(e.target.checked)} />
          Mark files viewed as I scroll past them
        </label>
        <label className="rv-check">
          <input type="checkbox" checked={hideViewed} onChange={(e) => setHideViewed(e.target.checked)} />
          Hide viewed files
        </label>
      </div>
      <div className="rv-panel-filter">
        <input className="rv-input" placeholder="Filter changed files" value={filter} onChange={(e) => setFilter(e.target.value)} />
      </div>
      <div className="rv-changes">
        {shown.map((f) => {
          const { dir, name } = splitPath(f.filename);
          const threads = pr.threads.filter((t) => t.path === f.filename && !t.resolved).length;
          return (
            <div key={f.filename} className={cx("rv-change", isViewed(f) && "viewed", active === f.filename && "active")}>
              <input
                type="checkbox"
                title={isViewed(f) ? "Viewed" : "Mark as viewed"}
                checked={isViewed(f)}
                onChange={(e) => act({ type: "mark_viewed", path: f.filename, viewed: e.target.checked }).catch((err) => notify(String(err), "danger"))}
              />
              <button className="rv-change-main" onClick={() => showChange(f.filename)} title={f.filename}>
                <span className="rv-change-name">{name}</span>
                <span className="rv-change-dir subtle">{dir}</span>
              </button>
              {threads > 0 && <span className="rv-file-threads open" title={`${threads} open threads`}><Icon name="message" size={11} />{threads}</span>}
              <span className={cx("rv-status", `text-${statusTone[f.status] ?? "neutral"}`)}>{statusLetter[f.status] ?? "M"}</span>
            </div>
          );
        })}
        {!shown.length && <p className="subtle rv-pad">{hideViewed ? "Every file is viewed." : "No files match."}</p>}
      </div>
    </div>
  );
}

// --- Threads ---------------------------------------------------------------

export function ThreadsPanel() {
  const { pr, openFile } = useReview();
  const [show, setShow] = useState<"open" | "all">("open");
  const list = pr.threads.filter((t) => show === "all" || !t.resolved);
  return (
    <div className="rv-panel">
      <PanelHead title="Review threads">
        <div className="rv-seg">
          <button className={cx(show === "open" && "on")} onClick={() => setShow("open")}>Open</button>
          <button className={cx(show === "all" && "on")} onClick={() => setShow("all")}>All</button>
        </div>
      </PanelHead>
      <div className="rv-changes">
        {list.map((t) => {
          const c = t.comments[0];
          const { name } = splitPath(t.path);
          return (
            <button key={t.id} className="rv-thread-row" onClick={() => openFile(t.path, t.line ?? t.original_line ?? 1)}>
              <div className="rv-thread-row-head">
                <Icon name={t.resolved ? "check" : "message"} size={12} />
                <strong>{name}</strong>
                <span className="subtle">:{t.line ?? t.original_line ?? "?"}</span>
                <span className="rv-flex" />
                {t.comments.some((x) => x.pending) && <span className="rv-pill-mini warning">Pending</span>}
                {t.outdated && <span className="rv-pill-mini">Outdated</span>}
              </div>
              <div className="rv-thread-row-body subtle"><strong>{c?.author}</strong> {c?.body.slice(0, 140)}</div>
            </button>
          );
        })}
        {!list.length && <p className="subtle rv-pad">{show === "open" ? "No open threads." : "No review threads yet."}</p>}
      </div>
    </div>
  );
}
