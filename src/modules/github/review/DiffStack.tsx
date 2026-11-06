// Every changed file in one scrolling column, like github.com's Files tab.
// Diffs mount as they come near the viewport. With auto-read on, a file is
// marked viewed once you've scrolled past its end.

import { useEffect, useRef, useState } from "react";
import { cx } from "../../../design/components";
import { Icon } from "../../../design/icons";
import type { PrFile } from "../../../lib/types";
import { useReview } from "./context";
import { FileDiff } from "./FileDiff";
import { isViewed, splitPath, statusLetter, statusTone } from "./model";

export function FileHeader({ file, collapsed, onToggle, onOpen }: { file: PrFile; collapsed?: boolean; onToggle?: () => void; onOpen?: () => void }) {
  const { pr, act, notify } = useReview();
  const viewed = isViewed(file);
  const threads = pr.threads.filter((t) => t.path === file.filename);
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

function StackItem({ file, scrollRoot, register, autoMarked }: { file: PrFile; scrollRoot: React.RefObject<HTMLDivElement | null>; register: (path: string, el: HTMLElement | null, end: HTMLElement | null) => void; autoMarked: Set<string> }) {
  const { openFile } = useReview();
  const viewed = isViewed(file);
  const [collapsed, setCollapsed] = useState(viewed);
  const [near, setNear] = useState(false);
  const el = useRef<HTMLElement>(null);
  const end = useRef<HTMLDivElement>(null);
  const prevViewed = useRef(viewed);
  const lastHeight = useRef<number | undefined>(undefined);

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

  return (
    <section
      ref={(n) => {
        el.current = n;
        register(file.filename, n, end.current);
      }}
      className={cx("rv-stack-item", viewed && "viewed")}
      data-path={file.filename}
    >
      <FileHeader file={file} collapsed={collapsed} onToggle={() => setCollapsed((c) => !c)} onOpen={() => openFile(file.filename)} />
      {!collapsed && (near ? <FileDiff file={file} initialHeight={lastHeight.current} onHeight={(h) => { lastHeight.current = h; }} /> : <div className="rv-diff-placeholder" style={{ height: lastHeight.current ?? Math.min(1200, 40 + (file.additions + file.deletions) * 19) }} />)}
      {!collapsed && file.additions + file.deletions === 0 && <p className="subtle rv-pad">No text changes (binary file, mode change, or rename).</p>}
      <div ref={end} className="rv-stack-end" />
    </section>
  );
}

export function DiffStack({ files, autoRead, scrollTo }: { files: PrFile[]; autoRead: boolean; scrollTo: { path: string; nonce: number } | null }) {
  const { act } = useReview();
  const root = useRef<HTMLDivElement>(null);
  const items = useRef(new Map<string, HTMLElement>());
  const ends = useRef(new Map<string, HTMLElement>());
  const autoMarked = useRef(new Set<string>()).current;
  const register = (path: string, el: HTMLElement | null, end: HTMLElement | null) => {
    if (el) items.current.set(path, el);
    else items.current.delete(path);
    if (el && end) ends.current.set(path, end);
    else ends.current.delete(path);
  };

  // Auto-read: once a file's end scrolls up past the top of the view, it's read.
  const filesRef = useRef(files);
  filesRef.current = files;
  useEffect(() => {
    const r = root.current;
    if (!r || !autoRead) return;
    let frame = 0;
    const check = () => {
      frame = 0;
      const top = r.getBoundingClientRect().top;
      for (const f of filesRef.current) {
        if (isViewed(f) || autoMarked.has(f.filename)) continue;
        const end = ends.current.get(f.filename);
        if (end && end.getBoundingClientRect().bottom < top) {
          autoMarked.add(f.filename);
          act({ type: "mark_viewed", path: f.filename, viewed: true }).catch(() => autoMarked.delete(f.filename));
        }
      }
    };
    const on = () => (frame ||= requestAnimationFrame(check));
    r.addEventListener("scroll", on, { passive: true });
    return () => {
      r.removeEventListener("scroll", on);
      cancelAnimationFrame(frame);
    };
  }, [autoRead, act, autoMarked]);
  useEffect(() => {
    if (!scrollTo) return;
    const el = items.current.get(scrollTo.path);
    if (el && root.current) root.current.scrollTo({ top: el.offsetTop - 8, behavior: "smooth" });
  }, [scrollTo]);

  if (!files.length) return <div className="empty"><Icon name="file" size={22} /><p>No files changed.</p></div>;
  return (
    <div ref={root} className="rv-stack">
      {files.map((f) => <StackItem key={f.filename} file={f} scrollRoot={root} register={register} autoMarked={autoMarked} />)}
      <div className="rv-stack-done subtle">
        <Icon name="check" size={14} /> End of changes
      </div>
    </div>
  );
}
