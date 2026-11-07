import { useEffect, useRef, useState } from "react";
import { api } from "../lib/api";
import { fileName, isImagePath, useImageSrc, type ViewerImage } from "../lib/images";
import { Icon } from "./icons";

/**
 * Full-size image viewer over the whole app, opened with `openImages()`.
 * Scroll or +/- to zoom, drag to pan, ←/→ to step, Esc to close.
 */
export function ImageViewer() {
  const [state, setState] = useState<{ images: ViewerImage[]; index: number } | null>(null);
  const [zoom, setZoom] = useState(1);
  const [pan, setPan] = useState({ x: 0, y: 0 });
  const drag = useRef<{ x: number; y: number; px: number; py: number } | null>(null);

  useEffect(() => {
    const open = (e: Event) => {
      setState((e as CustomEvent).detail);
      setZoom(1);
      setPan({ x: 0, y: 0 });
    };
    window.addEventListener("tab:open-images", open);
    return () => window.removeEventListener("tab:open-images", open);
  }, []);

  const count = state?.images.length ?? 0;
  const step = (d: number) => {
    if (!state || count < 2) return;
    setState({ ...state, index: (state.index + d + count) % count });
    setZoom(1);
    setPan({ x: 0, y: 0 });
  };
  const zoomBy = (f: number) => setZoom((z) => Math.min(8, Math.max(0.25, z * f)));
  const reset = () => (setZoom(1), setPan({ x: 0, y: 0 }));

  useEffect(() => {
    if (!state) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setState(null);
      else if (e.key === "ArrowRight") step(1);
      else if (e.key === "ArrowLeft") step(-1);
      else if (e.key === "+" || e.key === "=") zoomBy(1.25);
      else if (e.key === "-") zoomBy(0.8);
      else if (e.key === "0") reset();
      else return;
      e.preventDefault();
      e.stopPropagation();
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  });

  const current = state?.images[state.index];
  const src = useImageSrc(current?.src);
  if (!state || !current) return null;

  return (
    <div
      className="iv"
      role="dialog"
      aria-label="Image viewer"
      onMouseDown={(e) => e.target === e.currentTarget && setState(null)}
      onWheel={(e) => zoomBy(e.deltaY < 0 ? 1.1 : 0.9)}
    >
      <div className="iv-bar">
        <span className="iv-name" title={current.src}>{current.name ?? fileName(current.src)}</span>
        {count > 1 && <span className="iv-count">{state.index + 1} of {count}</span>}
        <span className="iv-flex" />
        <button onClick={() => zoomBy(0.8)} aria-label="Zoom out" title="Zoom out (-)"><Icon name="minus" size={16} /></button>
        <button onClick={reset} className="iv-zoom" title="Fit (0)">{Math.round(zoom * 100)}%</button>
        <button onClick={() => zoomBy(1.25)} aria-label="Zoom in" title="Zoom in (+)"><Icon name="plus" size={16} /></button>
        <button onClick={() => setState(null)} aria-label="Close" title="Close (Esc)"><Icon name="x" size={16} /></button>
      </div>
      {count > 1 && <button className="iv-step prev" onClick={() => step(-1)} aria-label="Previous"><Icon name="arrow-left" size={18} /></button>}
      {count > 1 && <button className="iv-step next" onClick={() => step(1)} aria-label="Next"><Icon name="arrow-left" size={18} /></button>}
      {src ? (
        <img
          className="iv-img"
          src={src}
          alt={current.name ?? ""}
          draggable={false}
          style={{ transform: `translate(${pan.x}px, ${pan.y}px) scale(${zoom})`, cursor: zoom > 1 ? "grab" : "zoom-in" }}
          onClick={() => zoom === 1 && zoomBy(2)}
          onMouseDown={(e) => {
            if (zoom <= 1) return;
            e.preventDefault();
            drag.current = { x: e.clientX, y: e.clientY, px: pan.x, py: pan.y };
            const move = (m: MouseEvent) => drag.current && setPan({ x: drag.current.px + m.clientX - drag.current.x, y: drag.current.py + m.clientY - drag.current.y });
            const up = () => ((drag.current = null), window.removeEventListener("mousemove", move), window.removeEventListener("mouseup", up));
            window.addEventListener("mousemove", move);
            window.addEventListener("mouseup", up);
          }}
        />
      ) : (
        <div className="iv-loading">Loading…</div>
      )}
    </div>
  );
}

/** Small clickable previews of images; click one to open the viewer on it. */
export function Thumbs({ srcs, onRemove, size = 72 }: { srcs: string[]; onRemove?: (i: number) => void; size?: number }) {
  if (!srcs.length) return null;
  return (
    <div className="thumbs">
      {srcs.map((s, i) => (
        <Thumb key={s} src={s} size={size} onOpen={() => import("../lib/images").then((m) => m.openImages(srcs, i))} onRemove={onRemove && (() => onRemove(i))} />
      ))}
    </div>
  );
}

function Thumb({ src, size, onOpen, onRemove }: { src: string; size: number; onOpen: () => void; onRemove?: () => void }) {
  const url = useImageSrc(src);
  return (
    <span className="thumb" style={{ width: size, height: size }}>
      <button className="thumb-img" onClick={onOpen} title={fileName(src)}>
        {url ? <img src={url} alt="" /> : <Icon name="file" size={16} />}
      </button>
      {onRemove && <button className="thumb-x" onClick={onRemove} aria-label="Remove"><Icon name="x" size={11} /></button>}
    </span>
  );
}

/** A file that isn't an image: its name, opening in its default app on click. */
export function FileChip({ path, label, onRemove }: { path: string; label?: string; onRemove?: () => void }) {
  const [error, setError] = useState<string | null>(null);
  const name = label || fileName(path.startsWith("blob:") ? path.split("#").pop()! : path);
  return (
    <span className={error ? "file-chip error" : "file-chip"} title={error ?? path}>
      <button
        className="file-chip-open"
        onClick={(e) => {
          setError(null);
          api.fileOpen(path, e.altKey).catch((err) => setError(String(err)));
        }}
        onContextMenu={(e) => {
          e.preventDefault();
          api.fileOpen(path, true).catch((err) => setError(String(err)));
        }}
      >
        <Icon name="file" size={13} />
        <span>{name}</span>
      </button>
      {onRemove && <button className="thumb-x" onClick={onRemove} aria-label="Remove"><Icon name="x" size={11} /></button>}
    </span>
  );
}

/** Attached files: images as thumbnails, everything else as file chips. */
export function Attachments({ paths, size = 72, onRemove }: { paths: string[]; size?: number; onRemove?: (path: string) => void }) {
  const images = paths.filter(isImagePath);
  const files = paths.filter((p) => !isImagePath(p));
  return (
    <>
      <Thumbs srcs={images} size={size} onRemove={onRemove && ((i) => onRemove(images[i]))} />
      {files.length > 0 && (
        <div className="file-chips">
          {files.map((f) => <FileChip key={f} path={f} onRemove={onRemove && (() => onRemove(f))} />)}
        </div>
      )}
    </>
  );
}
