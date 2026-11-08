// Images in the review: the picture before and after, side by side, instead
// of their bytes as text. SVGs can flip to their source diff.

import { useState } from "react";
import { cx } from "../../../design/components";
import { Icon } from "../../../design/icons";
import type { PrFile } from "../../../lib/types";
import { useReview } from "./context";
import { useFileImage } from "./data";
import { FileDiff } from "./FileDiff";
import { FileView } from "./FileView";

const isSvg = (path: string) => /\.svg$/i.test(path);

function Pane({ label, tone, url, loading, error }: { label?: string; tone?: "danger" | "success"; url: string | null; loading: boolean; error: unknown }) {
  const [size, setSize] = useState<string | null>(null);
  return (
    <figure className="rv-image-pane">
      {label && <figcaption className={cx(tone && `text-${tone}`)}>{label}{size && <span className="subtle"> · {size}</span>}</figcaption>}
      <div className="rv-image-box">
        {error ? (
          <p className="rv-error">Couldn't load this image: {String(error)}</p>
        ) : loading ? (
          <div className="skeleton rv-image-loading" />
        ) : url ? (
          <img src={url} alt={label ?? ""} onLoad={(e) => setSize(`${e.currentTarget.naturalWidth} × ${e.currentTarget.naturalHeight}`)} />
        ) : (
          <p className="subtle">No preview (the file may be too big to fetch).</p>
        )}
      </div>
    </figure>
  );
}

function SourceToggle({ source, onToggle }: { source: boolean; onToggle: () => void }) {
  return (
    <div className="rv-image-tools">
      <button className="rv-icon-btn" onClick={onToggle} title={source ? "Show the image" : "Show the source"} aria-pressed={source}>
        <Icon name={source ? "image" : "code"} size={13} /> {source ? "Image" : "Source"}
      </button>
    </div>
  );
}

/** A changed image: the base and head versions, or just the one side that exists. */
export function ImageDiff({ file, fill }: { file: PrFile; fill?: boolean }) {
  const { src } = useReview();
  const [source, setSource] = useState(false);
  const base = useFileImage(src, "base", file.previous_filename ?? file.filename, file.status !== "added" && !source);
  const head = useFileImage(src, "head", file.filename, file.status !== "removed" && !source);
  const svg = isSvg(file.filename);
  if (svg && source)
    return (
      <div className={cx("rv-image-diff", fill && "rv-image-fill", "rv-image-source")}>
        <SourceToggle source onToggle={() => setSource(false)} />
        <FileDiff file={file} fill={fill} />
      </div>
    );
  return (
    <div className={cx("rv-image-diff", fill && "rv-image-fill")}>
      {svg && <SourceToggle source={false} onToggle={() => setSource(true)} />}
      <div className="rv-image-row">
        {file.status !== "added" && <Pane label={file.status === "removed" ? "Removed" : "Before"} tone="danger" {...base} />}
        {file.status !== "removed" && <Pane label={file.status === "added" ? "Added" : "After"} tone="success" {...head} />}
      </div>
    </div>
  );
}

/** An image from the explorer, at the PR's head. */
export function ImageFile({ path, reveal }: { path: string; reveal?: { line: number; column?: number; nonce: number } }) {
  const { src } = useReview();
  const [source, setSource] = useState(false);
  const head = useFileImage(src, "head", path, !source);
  const svg = isSvg(path);
  if (svg && source)
    return (
      <div className="rv-image-diff rv-image-fill rv-image-source">
        <SourceToggle source onToggle={() => setSource(false)} />
        <FileView path={path} reveal={reveal} />
      </div>
    );
  return (
    <div className="rv-image-diff rv-image-fill">
      {svg && <SourceToggle source={false} onToggle={() => setSource(true)} />}
      <div className="rv-image-row"><Pane {...head} /></div>
    </div>
  );
}
