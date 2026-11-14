import { useQuery, useQueryClient } from "@tanstack/react-query";
import { convertFileSrc } from "@tauri-apps/api/core";
import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { api, inTauri } from "../lib/api";
import { fileName, isImagePath, isLocalPath, MISSING_IMAGE, openImages, useImage, useImageSrc } from "../lib/images";
import { StageContext, useStage, type Stage, type StageFile } from "../lib/stage";
import type { ThreadFile } from "../lib/threadFiles";
import { Button, cx } from "./components";
import { Icon } from "./icons";
import { MarkdownFiles } from "./markdown";
import { RichMarkdown } from "./richMarkdown";

const WIDTH_KEY = "tab.stage.chatWidth";
const clampChat = (w: number) => Math.max(320, Math.min(w, Math.round(window.innerWidth * 0.6)));

const MARKDOWN = new Set(["md", "markdown"]);
const HTML = new Set(["html", "htm"]);
const VIDEO = new Set(["mp4", "m4v", "mov", "webm", "ogv"]);
const AUDIO = new Set(["mp3", "m4a", "wav", "ogg", "oga", "opus", "flac", "aac"]);
const TEXT = new Set([
  "txt", "csv", "tsv", "json", "jsonl", "log", "yaml", "yml", "toml", "xml", "css", "js", "jsx", "ts", "tsx", "mjs", "cjs",
  "rs", "py", "rb", "go", "java", "kt", "swift", "c", "h", "cc", "cpp", "hpp", "cs", "sh", "zsh", "bash", "sql", "ini", "env",
  "conf", "diff", "patch", "lua", "php", "vue", "svelte", "dart", "scala", "ex", "exs", "r", "tex",
]);
const MAX_TEXT = 2_000_000;
/** Text past this is only laid out when asked for: a multi-megabyte `<pre>` makes the whole pane crawl. */
const FIRST_TEXT = 200_000;

const extOf = (path: string) => /\.([A-Za-z0-9]+)$/.exec(path.split(/[?#]/)[0])?.[1].toLowerCase() ?? "";
const dirOf = (path: string) => path.replace(/[\\/][^\\/]*$/, "");

export type Kind = "image" | "video" | "audio" | "pdf" | "svg" | "markdown" | "html" | "text" | "other";
export function kindOf(path: string): Kind {
  const ext = extOf(path);
  if (isImagePath(path) || /^data:image\//.test(path)) return "image";
  if (VIDEO.has(ext)) return "video";
  if (AUDIO.has(ext)) return "audio";
  if (!isLocalPath(path)) return "other";
  if (ext === "pdf") return "pdf";
  if (ext === "svg") return "svg";
  if (MARKDOWN.has(ext)) return "markdown";
  if (HTML.has(ext)) return "html";
  if (TEXT.has(ext)) return "text";
  return "other";
}

const POLL_MS = 2_000;
const UPDATED_MS = 4_000;

/**
 * Wraps a chat (and anything beside it) so files opened from it take the
 * middle of the pane, with the chat docked on the right to keep talking.
 * The chat stays mounted throughout, so the draft and scroll survive.
 *
 * `files` are the chat's files: the stage steps through them, and while it
 * is mounted it watches them on disk, so a file the agent rewrites reloads
 * wherever it shows (here, the tray, the rail).
 */
export function StageHost({ className, files = [], children }: { className?: string; files?: ThreadFile[]; children: ReactNode }) {
  const [state, setState] = useState<{ path: string; list: StageFile[] | null } | null>(null);
  const [width, setWidth] = useState(() => {
    try { return clampChat(Number(localStorage.getItem(WIDTH_KEY)) || 440); } catch { return 440; }
  });
  const updated = useFileWatch(files, state?.path);

  // A path on the stage that the agent has since replaced shows its version.
  const byPath = useMemo(() => {
    const m = new Map<string, ThreadFile>();
    for (const f of files) {
      m.set(f.path, f);
      if (f.original) m.set(f.original, f);
    }
    return m;
  }, [files]);
  const filesRef = useRef(byPath);
  filesRef.current = byPath;
  const current = state ? byPath.get(state.path)?.path ?? state.path : null;
  const list: StageFile[] = state?.list ?? files;
  const index = Math.max(0, list.findIndex((f) => f.path === current));

  const open = useCallback((picked: (string | StageFile)[], i = 0) => {
    const all = picked.map((f) => (typeof f === "string" ? { path: f } : f));
    const path = all[i]?.path;
    if (!path) return;
    // One of the chat's files: step through all of them, kept current as the chat goes.
    setState({ path, list: filesRef.current.has(path) ? null : all });
  }, []);
  const stage = useMemo<Stage>(() => ({ open, files, current, updated }), [open, files, current, updated]);

  const hostRef = useRef<HTMLDivElement>(null);
  // While dragging, the width goes straight onto the element once per frame
  // instead of through React state on every mouse move; state catches up on
  // release. Re-rendering the pane per pixel is what made dragging stutter.
  const startDrag = useCallback((e: React.MouseEvent) => {
    e.preventDefault();
    const x0 = e.clientX;
    const w0 = clampChat(width);
    let last = w0;
    let frame = 0;
    const move = (m: MouseEvent) => {
      last = clampChat(w0 - (m.clientX - x0));
      if (!frame) frame = requestAnimationFrame(() => {
        frame = 0;
        hostRef.current?.style.setProperty("--stage-chat-w", `${last}px`);
      });
    };
    const up = () => {
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", up);
      cancelAnimationFrame(frame);
      document.body.classList.remove("resizing");
      setWidth(last);
      try { localStorage.setItem(WIDTH_KEY, String(last)); } catch { /* private mode */ }
    };
    document.body.classList.add("resizing");
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", up);
  }, [width]);
  const listRef = useRef(list);
  listRef.current = list;
  const onStep = useCallback((d: number) => setState((s) => {
    const l = listRef.current;
    if (!s || !l.length) return s;
    const at = l.findIndex((f) => f.path === (filesRef.current.get(s.path)?.path ?? s.path));
    return { ...s, path: l[(Math.max(0, at) + d + l.length) % l.length].path };
  }), []);
  const onPick = useCallback((path: string) => setState((s) => s && { ...s, path }), []);
  const onClose = useCallback(() => setState(null), []);
  const file = current ? list.find((f) => f.path === current) ?? { path: current } : null;

  return (
    <StageContext.Provider value={stage}>
      <div ref={hostRef} className={cx(className, "stage-host", state && "staged")} style={state ? ({ "--stage-chat-w": `${width}px` } as React.CSSProperties) : undefined}>
        {file && (
          <FileView
            file={file}
            files={list}
            index={index}
            replaced={byPath.get(current!)?.original}
            fresh={updated.has(file.path)}
            onStep={onStep}
            onPick={onPick}
            onClose={onClose}
            onResize={startDrag}
          />
        )}
        {children}
      </div>
    </StageContext.Provider>
  );
}

/**
 * Polls the chat's files (and the one on show) for changes on disk. A change
 * refetches that file's bytes, so every view of it reloads, and marks it
 * updated for a few seconds.
 */
function useFileWatch(files: ThreadFile[], extra: string | undefined): ReadonlySet<string> {
  const qc = useQueryClient();
  const [updated, setUpdated] = useState<ReadonlySet<string>>(() => new Set());
  const stamps = useRef(new Map<string, string>());
  const key = useMemo(() => {
    const paths = files.map((f) => f.path).filter(isLocalPath).slice(-63);
    if (extra && isLocalPath(extra) && !paths.includes(extra)) paths.push(extra);
    return paths.join("\n");
  }, [files, extra]);

  useEffect(() => {
    if (!inTauri || !key) return;
    const paths = key.split("\n");
    let alive = true;
    const timers: number[] = [];
    const check = async () => {
      if (document.hidden) return;
      const got = await api.fileStamps(paths).catch(() => null);
      if (!alive || !got) return;
      const changed: string[] = [];
      paths.forEach((p, i) => {
        const now = got[i] ? got[i]!.join(":") : "gone";
        const before = stamps.current.get(p);
        stamps.current.set(p, now);
        if (before !== undefined && before !== now) changed.push(p);
      });
      if (!changed.length) return;
      for (const p of changed) {
        qc.invalidateQueries({ queryKey: ["file", p] });
        qc.invalidateQueries({ queryKey: ["image", p] });
      }
      setUpdated((u) => new Set([...u, ...changed]));
      timers.push(window.setTimeout(() => alive && setUpdated((u) => new Set([...u].filter((p) => !changed.includes(p)))), UPDATED_MS));
    };
    void check();
    const id = window.setInterval(check, POLL_MS);
    return () => {
      alive = false;
      window.clearInterval(id);
      timers.forEach(clearTimeout);
    };
  }, [key, qc]);
  return updated;
}

function FileView({ file, files, index, replaced, fresh, onStep, onPick, onClose, onResize }: { file: StageFile; files: StageFile[]; index: number; replaced?: string; fresh: boolean; onStep: (d: number) => void; onPick: (path: string) => void; onClose: () => void; onResize: (e: React.MouseEvent) => void }) {
  const kind = kindOf(file.path);
  const name = file.name || fileName(file.path);
  const local = isLocalPath(file.path);
  const [error, setError] = useState<string | null>(null);
  const [source, setSource] = useState(false);
  const ref = useRef<HTMLElement>(null);

  useEffect(() => { setError(null); setSource(false); }, [file.path]);
  useEffect(() => ref.current?.focus(), []);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const t = e.target as HTMLElement | null;
      if (t instanceof HTMLInputElement || t instanceof HTMLTextAreaElement || t instanceof HTMLMediaElement || t?.isContentEditable) return;
      if (document.querySelector(".iv")) return; // the full-screen viewer has the keys
      if (e.key === "Escape") onClose();
      else if (e.key === "ArrowRight" && files.length > 1) onStep(1);
      else if (e.key === "ArrowLeft" && files.length > 1) onStep(-1);
      else return;
      e.preventDefault();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [files.length, onClose, onStep]);

  const openExternally = (reveal: boolean) => api.fileOpen(file.path, reveal).catch((e) => setError(String(e)));

  return (
    <section className="stage-file" ref={ref} tabIndex={-1} aria-label={`Viewing ${name}`}>
      <header className="stage-bar">
        <Icon name={kind === "image" ? "image" : kind === "video" ? "video" : kind === "html" ? "globe" : kind === "text" ? "code" : "file"} size={14} />
        {files.length > 1 ? (
          <FileSwitcher files={files} current={file.path} name={name} onPick={onPick} />
        ) : (
          <span className="stage-name" title={file.path}>{name}</span>
        )}
        {replaced && <span className="stage-tag" title={`The agent's version of ${replaced}`}>Edited</span>}
        {fresh && <span className="stage-tag stage-tag-fresh">Updated</span>}
        {files.length > 1 && (
          <span className="stage-count">
            <button onClick={() => onStep(-1)} aria-label="Previous file" title="Previous (←)"><Icon name="arrow-left" size={13} /></button>
            {index + 1} of {files.length}
            <button onClick={() => onStep(1)} aria-label="Next file" title="Next (→)"><Icon name="arrow-right" size={13} /></button>
          </span>
        )}
        <span className="stage-flex" />
        {kind === "html" && (
          <button onClick={() => setSource((v) => !v)} title={source ? "Show the page" : "Show the source"} aria-pressed={source}>
            <Icon name={source ? "eye" : "code"} size={14} />
          </button>
        )}
        {kind === "image" && (
          <button onClick={() => openImages(files.filter((f) => kindOf(f.path) === "image").map((f) => ({ src: f.path, name: f.name })), Math.max(0, files.filter((f) => kindOf(f.path) === "image").findIndex((f) => f.path === file.path)))} title="Full screen"><Icon name="maximize" size={14} /></button>
        )}
        {local && inTauri && <button onClick={() => openExternally(false)} title="Open in its app"><Icon name="external" size={14} /></button>}
        {local && inTauri && <button onClick={() => openExternally(true)} title="Show in folder"><Icon name="folder-open" size={14} /></button>}
        <button onClick={onClose} aria-label="Close" title="Close (Esc)"><Icon name="x" size={14} /></button>
      </header>
      {error && <div className="chat-error"><Icon name="x-circle" size={14} /> {error}</div>}
      <div className="stage-content">
        {kind === "image" ? (
          <ImageContent file={file} />
        ) : kind === "video" || kind === "audio" ? (
          <MediaContent key={file.path} path={file.path} kind={kind} name={name} onOpen={local && inTauri ? () => openExternally(false) : undefined} />
        ) : kind === "other" ? (
          <OtherContent name={name} onOpen={() => openExternally(false)} onReveal={() => openExternally(true)} canOpen={local && inTauri} />
        ) : (
          <ReadContent key={file.path} path={file.path} kind={kind === "html" && source ? "text" : kind} />
        )}
      </div>
      <div className="stage-resize" onMouseDown={onResize} title="Drag to resize the chat" />
    </section>
  );
}

function ImageContent({ file }: { file: StageFile }) {
  const { url: src, failed } = useImage(file.path);
  if (failed) return <div className="stage-note">{MISSING_IMAGE}</div>;
  if (!src) return <div className="stage-note">Loading…</div>;
  return (
    <div className="stage-image">
      <img src={src} alt={file.name ?? ""} onClick={() => openImages([{ src: file.path, name: file.name }])} title="Click for full screen" />
    </div>
  );
}

/**
 * Video and audio play straight from disk through Tauri's asset protocol,
 * which streams with range requests, so seeking works and a long video
 * doesn't have to be read into memory first.
 */
function MediaContent({ path, kind, name, onOpen }: { path: string; kind: "video" | "audio"; name: string; onOpen?: () => void }) {
  const [failed, setFailed] = useState(false);
  const src = useMemo(() => (isLocalPath(path) && inTauri ? convertFileSrc(path) : path), [path]);
  if (failed)
    return (
      <div className="stage-other">
        <Icon name="video" size={36} />
        <strong>{name}</strong>
        <p className="subtle">This {kind} won't play here (its format may not be supported).</p>
        {onOpen && <div className="stage-other-actions"><Button size="sm" variant="primary" icon="external" onClick={onOpen}>Open in its app</Button></div>}
      </div>
    );
  return (
    <div className="stage-media">
      {kind === "video" ? (
        <video src={src} controls preload="metadata" playsInline onError={() => setFailed(true)} />
      ) : (
        <audio src={src} controls preload="metadata" onError={() => setFailed(true)} />
      )}
    </div>
  );
}

/**
 * Content-Security-Policy for pages shown in the HTML frame. Scripts, styles,
 * fonts and media may come inline or from https (pages an agent writes often
 * pull a chart library off a CDN); fetch/XHR/WebSockets, forms and nested
 * frames are blocked so a page can't send anything anywhere on its own.
 */
const HTML_CSP = [
  "default-src 'none'",
  "script-src 'unsafe-inline' 'unsafe-eval' https: data: blob:",
  "style-src 'unsafe-inline' https: data:",
  "img-src https: data: blob:",
  "font-src https: data:",
  "media-src https: data: blob:",
  "worker-src blob:",
  "connect-src 'none'",
  "frame-src 'none'",
  "form-action 'none'",
  "base-uri 'none'",
].join("; ");

/** `html` with the policy above as the first thing in its head. */
function withCsp(html: string): string {
  const meta = `<meta http-equiv="Content-Security-Policy" content="${HTML_CSP}">`;
  const doctype = /^\s*<!doctype[^>]*>/i.exec(html);
  return doctype ? doctype[0] + meta + html.slice(doctype[0].length) : meta + html;
}

/**
 * Renders an HTML file in a sandboxed frame. `allow-scripts` without
 * `allow-same-origin` gives the page an opaque origin: its JavaScript runs,
 * but it can't touch tab's window, storage, cookies or the Tauri bridge, open
 * popups, navigate tab, or submit forms. The CSP keeps it off the network.
 */
function HtmlFrame({ html, title }: { html: string; title: string }) {
  const doc = useMemo(() => withCsp(html), [html]);
  return (
    <iframe
      className="stage-html"
      srcDoc={doc}
      title={title}
      sandbox="allow-scripts allow-modals"
      allow=""
      referrerPolicy="no-referrer"
    />
  );
}

function TextContent({ text }: { text: string }) {
  const [all, setAll] = useState(false);
  const long = text.length > FIRST_TEXT;
  return (
    <>
      <pre className="stage-text">{long && !all ? text.slice(0, FIRST_TEXT) : text}</pre>
      {long && !all && (
        <div className="stage-more">
          <Button size="sm" onClick={() => setAll(true)}>Show the rest ({Math.round((text.length - FIRST_TEXT) / 1000)}k more characters)</Button>
        </div>
      )}
    </>
  );
}

function ReadContent({ path, kind }: { path: string; kind: Exclude<Kind, "image" | "video" | "audio" | "other"> }) {
  const { data, error } = useQuery({
    queryKey: ["file", path],
    queryFn: () => api.fileRead(path),
    staleTime: 5_000,
    gcTime: 60_000,
  });
  const url = useMemo(
    () => (data && (kind === "pdf" || kind === "svg") ? URL.createObjectURL(new Blob([data], { type: kind === "pdf" ? "application/pdf" : "image/svg+xml" })) : null),
    [data, kind],
  );
  useEffect(() => () => { if (url) URL.revokeObjectURL(url); }, [url]);
  const text = useMemo(() => {
    if (!data || kind === "pdf" || kind === "svg") return null;
    const t = new TextDecoder().decode(data);
    return t.length > MAX_TEXT ? t.slice(0, MAX_TEXT) + "\n\n… (cut short: open it in its app to see the rest)" : t;
  }, [data, kind]);

  if (error) return <div className="stage-note">{String(error)}</div>;
  if (!data) return <div className="stage-note">Loading…</div>;
  if (kind === "pdf") return <iframe className="stage-pdf" src={url!} title={fileName(path)} />;
  if (kind === "svg") return <div className="stage-image"><img src={url!} alt="" /></div>;
  if (kind === "html") return <HtmlFrame html={text ?? ""} title={fileName(path)} />;
  if (kind === "markdown")
    return (
      <div className="stage-doc">
        <MarkdownFiles.Provider value={{ base: dirOf(path) }}>
          <RichMarkdown text={text ?? ""} />
        </MarkdownFiles.Provider>
      </div>
    );
  return <TextContent text={text ?? ""} />;
}

function OtherContent({ name, canOpen, onOpen, onReveal }: { name: string; canOpen: boolean; onOpen: () => void; onReveal: () => void }) {
  return (
    <div className="stage-other">
      <Icon name="file" size={36} />
      <strong>{name}</strong>
      <p className="subtle">tab can't show this kind of file here.</p>
      {canOpen && (
        <div className="stage-other-actions">
          <Button size="sm" variant="primary" icon="external" onClick={onOpen}>Open in its app</Button>
          <Button size="sm" icon="folder-open" onClick={onReveal}>Show in folder</Button>
        </div>
      )}
    </div>
  );
}

const KIND_ICON: Record<Kind, string> = { image: "image", video: "video", audio: "video", pdf: "file", svg: "image", markdown: "file", html: "globe", text: "code", other: "file" };

/** The file's name in the stage bar; a menu of the chat's other files to swap to. */
function FileSwitcher({ files, current, name, onPick }: { files: StageFile[]; current: string; name: string; onPick: (path: string) => void }) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLSpanElement>(null);
  useEffect(() => {
    if (!open) return;
    const away = (e: MouseEvent) => ref.current && !ref.current.contains(e.target as Node) && setOpen(false);
    const esc = (e: KeyboardEvent) => e.key === "Escape" && (e.stopPropagation(), setOpen(false));
    window.addEventListener("mousedown", away);
    window.addEventListener("keydown", esc, true);
    return () => (window.removeEventListener("mousedown", away), window.removeEventListener("keydown", esc, true));
  }, [open]);
  return (
    <span className="menu-anchor stage-switch" ref={ref}>
      <button className="stage-name stage-name-btn" title={current} aria-haspopup="menu" aria-expanded={open} onClick={() => setOpen((v) => !v)}>
        <span>{name}</span>
        <Icon name="chevron-down" size={12} />
      </button>
      {open && (
        <div className="menu stage-menu" role="menu">
          {files.map((f) => (
            <button key={f.path} role="menuitemradio" aria-checked={f.path === current} className={cx("menu-item", f.path === current && "active")} title={f.path} onClick={() => (onPick(f.path), setOpen(false))}>
              <FilePreview path={f.path} size="xs" />
              <span>{f.name || fileName(f.path)}</span>
            </button>
          ))}
        </div>
      )}
    </span>
  );
}

/**
 * A small look at a file: images as a thumbnail, text, markdown and code as
 * their first lines, anything else as its kind's icon and extension.
 */
export function FilePreview({ path, size = "md" }: { path: string; size?: "xs" | "sm" | "md" }) {
  const kind = kindOf(path);
  const textual = kind === "text" || kind === "markdown";
  const img = useImageSrc(kind === "image" ? path : null);
  const { data } = useQuery({
    queryKey: ["file", path],
    queryFn: () => api.fileRead(path),
    enabled: textual && size !== "xs",
    staleTime: 5_000,
    gcTime: 60_000,
  });
  const snippet = useMemo(() => (data ? new TextDecoder().decode(data.slice(0, 1200)) : null), [data]);
  return (
    <span className={cx("file-preview", `file-preview-${size}`)} aria-hidden>
      {img ? (
        <img src={img} alt="" />
      ) : snippet && size !== "xs" ? (
        <span className="file-preview-text">{snippet}</span>
      ) : (
        <>
          <Icon name={KIND_ICON[kind]} size={size === "xs" ? 12 : 16} />
          {size !== "xs" && extOf(path) && <span className="file-preview-ext">{extOf(path)}</span>}
        </>
      )}
    </span>
  );
}

/**
 * The chat's files pinned above the composer, as small previews: click one
 * to put it on the stage. The one on show is outlined; one that just
 * changed on disk pulses.
 */
export function FileTray() {
  const stage = useStage();
  if (!stage?.files.length) return null;
  const { files, current, updated, open } = stage;
  return (
    <div className="file-tray" role="list" aria-label="Files in this chat">
      {files.map((f, i) => (
        <button
          key={f.path}
          role="listitem"
          className={cx("file-tile", f.path === current && "active", updated.has(f.path) && "fresh")}
          title={`${f.path}${f.original ? "\nThe agent's version of " + f.original : ""}`}
          onClick={(e) => {
            if (e.altKey && inTauri) return void api.fileOpen(f.path, true).catch(() => {});
            open(files, i);
          }}
        >
          <FilePreview path={f.path} size="sm" />
          <span className="file-tile-name">{f.name}</span>
          {f.original && <span className="file-tile-tag">Edited</span>}
        </button>
      ))}
    </div>
  );
}
