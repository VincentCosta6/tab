// Every changed file in one scrolling column, like github.com's Files tab.
// Diffs mount as they come near the viewport. With auto-read on, a file is
// marked viewed once you've scrolled past its end.
//
// Scrolling has to stay cheap with several Monaco editors alive, so the stack
// owns the only scroll listener: pinned diffs join it (StackCtx), all of them
// measure, then all of them scroll their editors, so layout is computed once
// per scroll event rather than once per editor. It runs in the scroll event
// itself, not a later frame, so Monaco paints the new lines in the same frame
// the page moved.

import { useEffect, useMemo, useRef, useState } from "react";
import { cx } from "../../../design/components";
import { Icon } from "../../../design/icons";
import type { PrFile, ReviewThread } from "../../../lib/types";
import { StackCtx, useReview, type StackFollower, type StackScroll } from "./context";
import { FileDiff } from "./FileDiff";
import { isViewed, splitPath, statusLetter, statusTone } from "./model";

// Creating a diff editor (models, the diff, decorations, zones) is the costliest
// thing this view does. Scrolling into a run of new files would create several
// in one frame and stall, so creation is queued and done one per idle slot.
const mountQueue: (() => void)[] = [];
let mountPending = false;
const later = (fn: () => void) =>
  typeof requestIdleCallback === "function" ? requestIdleCallback(fn, { timeout: 100 }) : setTimeout(fn, 16);
function pump() {
  mountPending = false;
  mountQueue.shift()?.();
  if (mountQueue.length) {
    mountPending = true;
    later(pump);
  }
}
function queueMount(fn: () => void) {
  mountQueue.push(fn);
  if (!mountPending) {
    mountPending = true;
    later(pump);
  }
  return () => {
    const i = mountQueue.indexOf(fn);
    if (i >= 0) mountQueue.splice(i, 1);
  };
}

// Real diff heights, kept after an editor is let go so its placeholder (and the
// next mount) is the right size and the page doesn't jump when files come and go.
const knownHeights = new Map<string, number>();
const heightKey = (f: PrFile, mode: string) => `${mode}:${f.filename}:${f.additions}:${f.deletions}`;

export function FileHeader({ file, threads: given, collapsed, onToggle, onOpen }: { file: PrFile; threads?: ReviewThread[]; collapsed?: boolean; onToggle?: () => void; onOpen?: () => void }) {
  const { pr, act, notify } = useReview();
  const viewed = isViewed(file);
  const threads = given ?? pr.threads.filter((t) => t.path === file.filename);
  const open = threads.filter((t) => !t.resolved).length;
  const { dir, name } = splitPath(file.filename);
  return (
    <div className="rv-file-head">
      {onToggle && (
        <button className="rv-icon-btn" onClick={onToggle} title={collapsed ? "Expand" : "Collapse"}>
          <Icon name={collapsed ? "chevron-right" : "chevron-down"} size={14} />
        </button>
      )}
      <span className={cx("rv-status", `text-${statusTone[file.status] ?? "neutral"}`)} title={file.status}>{statusLetter[file.status] ?? "M"}</span>
      <button className="rv-file-path" onClick={onOpen} title={file.filename}>
        {file.previous_filename && file.previous_filename !== file.filename && <span className="subtle">{file.previous_filename} → </span>}
        {dir && <span className="subtle">{dir}/</span>}
        <strong>{name}</strong>
      </button>
      <span className="rv-file-stat"><span className="text-success">+{file.additions}</span> <span className="text-danger">−{file.deletions}</span></span>
      {threads.length > 0 && (
        <span className={cx("rv-file-threads", open > 0 && "open")} title={`${open} open of ${threads.length} threads`}>
          <Icon name="message" size={12} /> {threads.length}
        </span>
      )}
      <span className="rv-flex" />
      {file.viewed === "dismissed" && <span className="rv-changed-since">Changed since last view</span>}
      <label className={cx("rv-viewed", viewed && "on")}>
        <input
          type="checkbox"
          checked={viewed}
          onChange={(e) => act({ type: "mark_viewed", path: file.filename, viewed: e.target.checked }).catch((err) => notify(String(err), "danger"))}
        />
        Viewed
      </label>
    </div>
  );
}

const NO_THREADS: ReviewThread[] = [];

function StackItem({ file, threads, scrollRoot, register, autoMarked }: { file: PrFile; threads: ReviewThread[]; scrollRoot: React.RefObject<HTMLDivElement | null>; register: (path: string, el: HTMLElement | null, end: HTMLElement | null) => void; autoMarked: Set<string> }) {
  const { openFile, mode } = useReview();
  const viewed = isViewed(file);
  const [collapsed, setCollapsed] = useState(viewed);
  const [near, setNear] = useState(false);
  const [mounted, setMounted] = useState(false);
  const el = useRef<HTMLElement>(null);
  const end = useRef<HTMLDivElement>(null);
  const prevViewed = useRef(viewed);
  const hKey = heightKey(file, mode);
  const known = knownHeights.get(hKey);

  // Marking a file viewed by hand collapses it, like github.com. Auto-read
  // leaves it open: it's above the view, and collapsing it would jump the page.
  useEffect(() => {
    if (viewed && !prevViewed.current && !autoMarked.has(file.filename)) setCollapsed(true);
    if (!viewed) setCollapsed(false);
    autoMarked.delete(file.filename);
    prevViewed.current = viewed;
  }, [viewed, autoMarked, file.filename]);

  useEffect(() => {
    const root = scrollRoot.current;
    if (!el.current || !root) return;
    // Mount diffs as they near the view and let go of them once far away, so
    // a PR with hundreds of files only ever keeps a few editors alive.
    const io = new IntersectionObserver((es) => setNear(es[es.length - 1].isIntersecting), { root, rootMargin: "1500px 0px" });
    io.observe(el.current);
    return () => io.disconnect();
  }, [scrollRoot]);

  // Near the view: get in line for an editor. Far away: let it go right away.
  useEffect(() => {
    if (!near || collapsed) return setMounted(false);
    return queueMount(() => setMounted(true));
  }, [near, collapsed]);

  return (
    <section
      ref={(n) => {
        el.current = n;
        register(file.filename, n, end.current);
      }}
      className={cx("rv-stack-item", viewed && "viewed")}
      data-path={file.filename}
    >
      <FileHeader file={file} threads={threads} collapsed={collapsed} onToggle={() => setCollapsed((c) => !c)} onOpen={() => openFile(file.filename)} />
      {!collapsed && (near && mounted ? <FileDiff file={file} initialHeight={known} onHeight={(h) => knownHeights.set(hKey, h)} /> : <div className="rv-diff-placeholder" style={{ height: known ?? Math.min(1200, 40 + (file.additions + file.deletions) * 19) }} />)}
      {!collapsed && file.additions + file.deletions === 0 && <p className="subtle rv-pad">No text changes (binary file, mode change, or rename).</p>}
      <div ref={end} className="rv-stack-end" />
    </section>
  );
}

export function DiffStack({ files, autoRead, scrollTo }: { files: PrFile[]; autoRead: boolean; scrollTo: { path: string; nonce: number } | null }) {
  const { act, pr } = useReview();
  const root = useRef<HTMLDivElement>(null);
  const byPath = useMemo(() => {
    const m = new Map<string, ReviewThread[]>();
    for (const t of pr.threads) {
      const list = m.get(t.path);
      if (list) list.push(t);
      else m.set(t.path, [t]);
    }
    return m;
  }, [pr.threads]);
  const followers = useRef(new Set<StackFollower>()).current;
  const stack = useMemo<StackScroll>(() => ({
    add: (f) => {
      followers.add(f);
      return () => followers.delete(f);
    },
  }), [followers]);
  const items = useRef(new Map<string, HTMLElement>());
  const ends = useRef(new Map<string, HTMLElement>());
  const autoMarked = useRef(new Set<string>()).current;
  const register = (path: string, el: HTMLElement | null, end: HTMLElement | null) => {
    if (el) items.current.set(path, el);
    else items.current.delete(path);
    if (el && end) ends.current.set(path, end);
    else ends.current.delete(path);
  };

  // The one scroll listener. Reads first (pinned editors, auto-read), then writes.
  // Auto-read: once a file's end scrolls up past the top of the view, it's read.
  // The last few files can never scroll that far, so once you reach the bottom
  // of the stack, every file whose end is on screen counts as read too.
  const filesRef = useRef(files);
  filesRef.current = files;
  useEffect(() => {
    const r = root.current;
    if (!r) return;
    const on = () => {
      for (const f of followers) f.read();
      if (autoRead) {
        const box = r.getBoundingClientRect();
        const atBottom = r.scrollTop > 0 && r.scrollTop + r.clientHeight >= r.scrollHeight - 4;
        const line = atBottom ? box.bottom : box.top;
        for (const f of filesRef.current) {
          if (isViewed(f) || autoMarked.has(f.filename)) continue;
          const end = ends.current.get(f.filename);
          if (end && end.getBoundingClientRect().bottom <= line) {
            autoMarked.add(f.filename);
            act({ type: "mark_viewed", path: f.filename, viewed: true }).catch(() => autoMarked.delete(f.filename));
          }
        }
      }
      for (const f of followers) f.write();
    };
    r.addEventListener("scroll", on, { passive: true });
    return () => r.removeEventListener("scroll", on);
  }, [autoRead, act, autoMarked, followers]);
  useEffect(() => {
    if (!scrollTo) return;
    const el = items.current.get(scrollTo.path);
    if (el && root.current) root.current.scrollTo({ top: el.offsetTop - 8, behavior: "smooth" });
  }, [scrollTo]);

  if (!files.length) return <div className="empty"><Icon name="file" size={22} /><p>No files changed.</p></div>;
  return (
    <StackCtx.Provider value={stack}>
      <div ref={root} className="rv-stack">
        {files.map((f) => <StackItem key={f.filename} file={f} threads={byPath.get(f.filename) ?? NO_THREADS} scrollRoot={root} register={register} autoMarked={autoMarked} />)}
        <div className="rv-stack-done subtle">
          <Icon name="check" size={14} /> End of changes
        </div>
      </div>
    </StackCtx.Provider>
  );
}
