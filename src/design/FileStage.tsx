import { useQuery } from "@tanstack/react-query";
import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { api, inTauri } from "../lib/api";
import { fileName, isImagePath, isLocalPath, openImages, useImageSrc } from "../lib/images";
import { StageContext, type Stage, type StageFile } from "../lib/stage";
import { Button, cx } from "./components";
import { Icon } from "./icons";
import { Markdown, MarkdownFiles } from "./markdown";

const WIDTH_KEY = "tab.stage.chatWidth";
const clampChat = (w: number) => Math.max(320, Math.min(w, Math.round(window.innerWidth * 0.6)));

const MARKDOWN = new Set(["md", "markdown"]);
const TEXT = new Set([
  "txt", "csv", "tsv", "json", "jsonl", "log", "yaml", "yml", "toml", "xml", "html", "css", "js", "jsx", "ts", "tsx", "mjs", "cjs",
  "rs", "py", "rb", "go", "java", "kt", "swift", "c", "h", "cc", "cpp", "hpp", "cs", "sh", "zsh", "bash", "sql", "ini", "env",
  "conf", "diff", "patch", "lua", "php", "vue", "svelte", "dart", "scala", "ex", "exs", "r", "tex",
]);
const MAX_TEXT = 2_000_000;

const extOf = (path: string) => /\.([A-Za-z0-9]+)$/.exec(path.split(/[?#]/)[0])?.[1].toLowerCase() ?? "";
const dirOf = (path: string) => path.replace(/[\\/][^\\/]*$/, "");

type Kind = "image" | "pdf" | "svg" | "markdown" | "text" | "other";
function kindOf(path: string): Kind {
  const ext = extOf(path);
  if (isImagePath(path) || /^data:image\//.test(path)) return "image";
  if (!isLocalPath(path)) return "other";
  if (ext === "pdf") return "pdf";
  if (ext === "svg") return "svg";
  if (MARKDOWN.has(ext)) return "markdown";
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

  const startDrag = (e: React.MouseEvent) => {
    e.preventDefault();
    const x0 = e.clientX;
    const w0 = width;
    let last = w0;
    const move = (m: MouseEvent) => setWidth((last = clampChat(w0 - (m.clientX - x0))));
    const up = () => {
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", up);
      document.body.classList.remove("resizing");
      try { localStorage.setItem(WIDTH_KEY, String(last)); } catch { /* private mode */ }
    };
    document.body.classList.add("resizing");
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", up);
  };

  return (
    <StageContext.Provider value={stage}>
      <div className={cx(className, "stage-host", state && "staged")} style={state ? ({ "--stage-chat-w": `${width}px` } as React.CSSProperties) : undefined}>
        {state && (
          <FileView
            files={state.files}
            index={state.index}
            onStep={(d) => setState((s) => s && { ...s, index: (s.index + d + s.files.length) % s.files.length })}
            onClose={() => setState(null)}
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
  const ref = useRef<HTMLElement>(null);

  useEffect(() => setError(null), [file.path]);
  useEffect(() => ref.current?.focus(), []);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const t = e.target as HTMLElement | null;
      if (t instanceof HTMLInputElement || t instanceof HTMLTextAreaElement || t?.isContentEditable) return;
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
        <Icon name={kind === "image" ? "image" : kind === "text" ? "code" : "file"} size={14} />
        <span className="stage-name" title={file.path}>{name}</span>
        {files.length > 1 && (
          <span className="stage-count">
            <button onClick={() => onStep(-1)} aria-label="Previous file" title="Previous (←)"><Icon name="arrow-left" size={13} /></button>
            {index + 1} of {files.length}
            <button onClick={() => onStep(1)} aria-label="Next file" title="Next (→)"><Icon name="arrow-right" size={13} /></button>
          </span>
        )}
        <span className="stage-flex" />
        {kind === "image" && (
          <button onClick={() => openImages(files.filter((f) => kindOf(f.path) === "image").map((f) => ({ src: f.path, name: f.name })), Math.max(0, files.filter((f) => kindOf(f.path) === "image").indexOf(file)))} title="Full screen"><Icon name="maximize" size={14} /></button>
        )}
        {local && inTauri && <button onClick={() => openExternally(false)} title="Open in its app"><Icon name="external" size={14} /></button>}
        {local && inTauri && <button onClick={() => openExternally(true)} title="Show in folder"><Icon name="folder-open" size={14} /></button>}
        <button onClick={onClose} aria-label="Close" title="Close (Esc)"><Icon name="x" size={14} /></button>
      </header>
      {error && <div className="chat-error"><Icon name="x-circle" size={14} /> {error}</div>}
      <div className="stage-content">
        {kind === "image" ? <ImageContent file={file} /> : kind === "other" ? <OtherContent name={name} onOpen={() => openExternally(false)} onReveal={() => openExternally(true)} canOpen={local && inTauri} /> : <ReadContent key={file.path} path={file.path} kind={kind} />}
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

function ReadContent({ path, kind }: { path: string; kind: Exclude<Kind, "image" | "other"> }) {
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
  if (kind === "markdown")
    return (
      <div className="stage-doc">
        <MarkdownFiles.Provider value={{ base: dirOf(path) }}>
          <Markdown text={text ?? ""} />
        </MarkdownFiles.Provider>
      </div>
    );
  return <pre className="stage-text">{text}</pre>;
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
