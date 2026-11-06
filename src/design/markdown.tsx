// Tiny, safe markdown: paragraphs, bullet lists, headings, `code`, **bold**,
// *italic* and links. No HTML passthrough. Enough for PR bodies and comments
// until we need a real renderer.

import { createContext, useContext, type MouseEvent, type ReactNode } from "react";
import { openLink } from "../lib/links";
import { isImagePath, isLocalPath, useImageSrc } from "../lib/images";
import { openFile, toViewerFile } from "../lib/viewer";
import { Icon } from "./icons";

/**
 * Handles links that aren't http(s), such as `src/a.ts#L12` in an agent's
 * reply about a repository. Without one, those links show as plain text.
 */
export const MarkdownLinks = createContext<((href: string, e: MouseEvent) => void) | null>(null);

function LocalLink({ href, children }: { href: string; children: ReactNode }) {
  const on = useContext(MarkdownLinks);
  if (!on) return <>{children}</>;
  return (
    <a href={href} className="md-code-link" title={href} onClick={(e) => { e.preventDefault(); on(href, e); }}>
      {children}
    </a>
  );
}

/** An image in a message: a thumbnail that opens the viewer. */
function InlineImage({ src, alt }: { src: string; alt: string }) {
  const url = useImageSrc(/^(https?:|data:|blob:)/.test(src) || isLocalPath(src) ? src : null);
  if (!url) return <code>{alt || src}</code>;
  return (
    <button className="md-image" onClick={() => { const f = toViewerFile(src, alt || undefined); if (f) openFile(f); }} title={alt || src}>
      <img src={url} alt={alt} />
    </button>
  );
}

/** A file the agent left (video, page, PDF, notes): a chip that opens it in the viewer. */
function FileChip({ src, label }: { src: string; label?: string }) {
  const f = toViewerFile(src, label);
  if (!f) return <code>{src}</code>;
  return (
    <button className="file-chip" onClick={() => openFile(f)} title={src}>
      <Icon name={f.kind === "video" ? "video" : f.kind === "html" ? "code" : "file"} size={13} />
      <span>{f.name}</span>
    </button>
  );
}

/** Short label for a bare link. Add a rule per service, as in crates/tab-core/src/links.rs. */
const LINK_LABELS: Array<(u: URL) => string | null> = [
  (u) => (u.hostname === "github.com" ? /^\/([^/]+\/[^/]+)\/(?:pull|issues)\/(\d+)\/?$/.exec(u.pathname)?.slice(1).join("#") ?? null : null),
];
export function linkLabel(href: string): string {
  try {
    const u = new URL(href);
    for (const rule of LINK_LABELS) { const l = rule(u); if (l) return l; }
    const path = u.pathname.replace(/\/$/, "");
    const text = u.hostname.replace(/^www\./, "") + path;
    return text.length > 48 ? text.slice(0, 47) + "…" : text;
  } catch { return href; }
}

function inline(text: string, key: string): ReactNode[] {
  const out: ReactNode[] = [];
  const re = /(!\[[^\]]*\]\([^)\s]+\)|`[^`]+`|\*\*[^*]+\*\*|\*[^*]+\*|\[[^\]]+\]\([^)\s]+\)|https?:\/\/[^\s<>()\[\]`]*[^\s<>()\[\]`.,;:!?'"])/g;
  let last = 0;
  let m: RegExpExecArray | null;
  let i = 0;
  while ((m = re.exec(text))) {
    if (m.index > last) out.push(text.slice(last, m.index));
    const t = m[0];
    const k = `${key}-${i++}`;
    if (t.startsWith("![")) {
      const [, alt, src] = /!\[([^\]]*)\]\(([^)\s]+)\)/.exec(t)!;
      out.push(<InlineImage key={k} src={src} alt={alt} />);
    } else if (t.startsWith("`") && isImagePath(t.slice(1, -1)) && isLocalPath(t.slice(1, -1))) {
      // A screenshot the agent saved, named by path: show it.
      out.push(<InlineImage key={k} src={t.slice(1, -1)} alt={t.slice(1, -1)} />);
    } else if (t.startsWith("`") && isLocalPath(t.slice(1, -1)) && toViewerFile(t.slice(1, -1))) {
      out.push(<FileChip key={k} src={t.slice(1, -1)} />);
    } else if (t.startsWith("`")) out.push(<code key={k}>{t.slice(1, -1)}</code>);
    else if (t.startsWith("**")) out.push(<strong key={k}>{t.slice(2, -2)}</strong>);
    else if (/^https?:\/\//.test(t) && toViewerFile(t) && toViewerFile(t)!.kind !== "html") out.push(<FileChip key={k} src={t} />);
    else if (/^https?:\/\//.test(t)) {
      // A bare link reads as a pill: owner/repo#12 for a PR, host/path for anything else.
      out.push(<a key={k} href={t} className="pr-pill" target="_blank" rel="noreferrer" title={t} onClick={(e) => { e.preventDefault(); void openLink(t, e); }}>{linkLabel(t)}</a>);
    } else if (t.startsWith("[")) {
      const [, label, href] = /\[([^\]]+)\]\(([^)\s]+)\)/.exec(t)!;
      if (toViewerFile(href) && (isLocalPath(href) || /^https?:/.test(href))) { out.push(<FileChip key={k} src={href} label={label} />); last = m.index + t.length; continue; }
      out.push(/^https?:\/\//.test(href) ? <a key={k} href={href} target="_blank" rel="noreferrer" title={href} onClick={(e) => { e.preventDefault(); void openLink(href, e); }}>{label}</a> : <LocalLink key={k} href={href}>{inline(label, k)}</LocalLink>);
    } else out.push(<em key={k}>{t.slice(1, -1)}</em>);
    last = m.index + t.length;
  }
  if (last < text.length) out.push(text.slice(last));
  return out;
}

export function Markdown({ text }: { text: string }) {
  const blocks = text.replace(/\r\n/g, "\n").split(/\n{2,}/);
  return (
    <div className="markdown">
      {blocks.map((b, i) => {
        const lines = b.split("\n");
        if (lines.every((l) => /^\s*[-*] /.test(l)))
          return <ul key={i}>{lines.map((l, j) => <li key={j}>{inline(l.replace(/^\s*[-*] /, ""), `${i}-${j}`)}</li>)}</ul>;
        const h = /^(#{1,4}) (.*)$/.exec(b);
        if (h && lines.length === 1) return <h4 key={i}>{inline(h[2], `${i}`)}</h4>;
        if (b.startsWith("```")) return <pre key={i}><code>{b.replace(/^```\w*\n?|```$/g, "")}</code></pre>;
        return <p key={i}>{lines.flatMap((l, j) => (j ? [<br key={`br${j}`} />, ...inline(l, `${i}-${j}`)] : inline(l, `${i}-${j}`)))}</p>;
      })}
    </div>
  );
}
