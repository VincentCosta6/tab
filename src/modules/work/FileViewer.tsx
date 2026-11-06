import { useEffect, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Icon } from "../../design/icons";
import { Markdown } from "../../design/markdown";
import { cx } from "../../design/components";
import { api, inTauri, openUrl } from "../../lib/api";
import { isLocalPath } from "../../lib/images";
import { closeViewer, mimeFor, selectFile, stepFile, useViewer, type FileKind, type ViewerFile } from "../../lib/viewer";

const icons: Record<FileKind, string> = { image: "image", video: "video", html: "code", pdf: "file", markdown: "file", text: "file" };

/** The file's bytes: read through tab when it's on disk, fetched when it's on the web. */
function useBytes(file: ViewerFile) {
  const local = isLocalPath(file.src);
  return useQuery({
    queryKey: ["file", file.src],
    queryFn: async () => {
      if (local) {
        if (!inTauri) throw new Error("Local files open in the desktop app.");
        return new Uint8Array(await api.fileRead(file.src));
      }
      const r = await fetch(file.src);
      if (!r.ok) throw new Error(`The address answered ${r.status}.`);
      return new Uint8Array(await r.arrayBuffer());
    },
    staleTime: Infinity,
    gcTime: 5 * 60_000,
    retry: false,
  });
}

function useBlobUrl(bytes: Uint8Array | undefined, type: string) {
  const [url, setUrl] = useState<string>();
  useEffect(() => {
    if (!bytes) return setUrl(undefined);
    const u = URL.createObjectURL(new Blob([bytes as BlobPart], { type }));
    setUrl(u);
    return () => URL.revokeObjectURL(u);
  }, [bytes, type]);
  return url;
}

function Body({ file }: { file: ViewerFile }) {
  const { data, error, isLoading } = useBytes(file);
  const url = useBlobUrl(file.kind === "image" || file.kind === "video" || file.kind === "pdf" ? data : undefined, mimeFor(file.src));
  const [actual, setActual] = useState(false);
  if (isLoading) return <div className="fv-note">Loading…</div>;
  if (error || !data) return <div className="fv-note">{(error as Error)?.message || "Couldn't open this file."}</div>;
  if (file.kind === "image") return url ? <div className={cx("fv-image", actual && "actual")}><img src={url} alt={file.name} onClick={() => setActual((v) => !v)} /></div> : null;
  if (file.kind === "video") return url ? <div className="fv-video"><video key={url} src={url} controls autoPlay={false} /></div> : null;
  if (file.kind === "pdf") return url ? <iframe className="fv-frame" title={file.name} src={url} /> : null;
  const text = new TextDecoder().decode(data);
  // The page runs with scripts but no access to tab: no same-origin, no top navigation.
  if (file.kind === "html") return <iframe className="fv-frame html" title={file.name} sandbox="allow-scripts allow-forms allow-modals allow-popups" srcDoc={text} />;
  if (file.kind === "markdown") return <div className="fv-text"><Markdown text={text} /></div>;
  return <pre className="fv-text fv-pre">{text}</pre>;
}

/** Shows one of the thread's files in the middle of the Work tab. */
export function FileViewer() {
  const { files, current } = useViewer();
  const file = files.find((f) => f.src === current);

  useEffect(() => {
    if (!file) return;
    const onKey = (e: KeyboardEvent) => {
      const t = e.target as HTMLElement;
      if (e.key === "Escape") closeViewer();
      else if (t instanceof HTMLInputElement || t instanceof HTMLTextAreaElement || t?.isContentEditable || e.metaKey || e.ctrlKey) return;
      else if (e.key === "ArrowLeft" && e.shiftKey) stepFile(-1);
      else if (e.key === "ArrowRight" && e.shiftKey) stepFile(1);
      else return;
      e.preventDefault();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [file, files.length]);

  if (!file) return null;
  const i = files.findIndex((f) => f.src === file.src);
  return (
    <section className="fv" aria-label="File viewer">
      <header className="fv-bar">
        <Icon name={icons[file.kind]} size={15} />
        <span className="fv-name" title={file.src}>{file.name}</span>
        {files.length > 1 && <span className="fv-count">{i + 1} of {files.length}</span>}
        <span className="fv-flex" />
        {files.length > 1 && (
          <>
            <button className="icon-btn" onClick={() => stepFile(-1)} title="Previous file (Shift ←)" aria-label="Previous file"><Icon name="arrow-left" size={15} /></button>
            <button className="icon-btn" onClick={() => stepFile(1)} title="Next file (Shift →)" aria-label="Next file"><Icon name="arrow-right" size={15} /></button>
          </>
        )}
        {!isLocalPath(file.src) && <button className="icon-btn" onClick={() => void openUrl(file.src)} title="Open in your browser" aria-label="Open in your browser"><Icon name="external" size={15} /></button>}
        <button className="icon-btn" onClick={closeViewer} title="Close (Esc)" aria-label="Close viewer"><Icon name="x" size={16} /></button>
      </header>
      {files.length > 1 && (
        <div className="fv-strip" role="tablist" aria-label="Files in this chat">
          {files.map((f) => (
            <button key={f.src} role="tab" aria-selected={f.src === file.src} className={cx("fv-chip", f.src === file.src && "active")} onClick={() => selectFile(f.src)} title={f.src}>
              <Icon name={icons[f.kind]} size={12} />
              <span>{f.name}</span>
            </button>
          ))}
        </div>
      )}
      <div className="fv-body"><Body key={file.src} file={file} /></div>
    </section>
  );
}
