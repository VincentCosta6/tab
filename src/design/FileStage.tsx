import { useQuery } from "@tanstack/react-query";
import { convertFileSrc } from "@tauri-apps/api/core";
import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { api, inTauri } from "../lib/api";
import { fileName, isImagePath, isLocalPath, openImages, useImageSrc } from "../lib/images";
import { StageContext, type Stage, type StageFile } from "../lib/stage";
import { Button, cx } from "./components";
import { Icon } from "./icons";
import { Markdown, MarkdownFiles } from "./markdown";

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

type Kind = "image" | "video" | "audio" | "pdf" | "svg" | "markdown" | "html" | "text" | "other";
function kindOf(path: string): Kind {
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

/**
 * Wraps a chat (and anything beside it) so files opened from it take the
 * middle of the pane, with the chat docked on the right to keep talking.
 * The chat stays mounted throughout, so the draft and scroll survive.
 */
export function StageHost({ className, children }: { className?: string; children: ReactNode }) {
  const [state, setState] = useState<{ files: StageFile[]; index: number } | null>(null);
  const [width, setWidth] = useState(() => {
    try { return clampChat(Number(localStorage.getItem(WIDTH_KEY)) || 440); } catch { return 440; }
  });
  const stage = useMemo<Stage>(() => ({
    open: (files, index = 0) => setState({ files: files.map((f) => (typeof f === "string" ? { path: f } : f)), index }),
  }), []);

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
  const onStep = useCallback((d: number) => setState((s) => s && { ...s, index: (s.index + d + s.files.length) % s.files.length }), []);
  const onClose = useCallback(() => setState(null), []);

  return (
    <StageContext.Provider value={stage}>
      <div ref={hostRef} className={cx(className, "stage-host", state && "staged")} style={state ? ({ "--stage-chat-w": `${width}px` } as React.CSSProperties) : undefined}>
        {state && (
          <FileView
            files={state.files}
            index={state.index}
            onStep={onStep}
            onClose={onClose}
            onResize={startDrag}
          />
        )}
        {children}
      </div>
    </StageContext.Provider>
  );
}

function FileView({ files, index, onStep, onClose, onResize }: { files: StageFile[]; index: number; onStep: (d: number) => void; onClose: () => void; onResize: (e: React.MouseEvent) => void }) {
  const file = files[index];
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
        <span className="stage-name" title={file.path}>{name}</span>
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
          <button onClick={() => openImages(files.filter((f) => kindOf(f.path) === "image").map((f) => ({ src: f.path, name: f.name })), Math.max(0, files.filter((f) => kindOf(f.path) === "image").indexOf(file)))} title="Full screen"><Icon name="maximize" size={14} /></button>
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
  const src = useImageSrc(file.path);
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
          <Markdown text={text ?? ""} />
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
