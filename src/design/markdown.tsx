// Tiny, safe markdown: paragraphs, bullet lists, headings, `code`, **bold**,
// *italic* and links. No HTML passthrough. Enough for PR bodies and comments
// until we need a real renderer.

import { createContext, useContext, type MouseEvent, type ReactNode } from "react";
import { openLink, parseLink } from "../lib/links";
import { LinkPill } from "./LinkPill";
import { isFilePath, isImagePath, isLocalPath, openImages, resolvePath, useImageSrc } from "../lib/images";
import { FileChip } from "./ImageViewer";
import { useStage } from "../lib/stage";

/**
 * Handles links that aren't http(s), such as `src/a.ts#L12` in an agent's
 * reply about a repository. Without one, those links show as plain text.
 */
export const MarkdownLinks = createContext<((href: string, e: MouseEvent) => void) | null>(null);

/**
 * The folder the agent works in, so a screenshot it names by a relative path
 * (`screenshots/after.png`) still shows. Files it names by an absolute path,
 * and links to local files, show as cards that open the file.
 */
export const MarkdownFiles = createContext<{ base: string | null; imagesAtEnd?: boolean } | null>(null);

function LocalLink({ href, label, children }: { href: string; label: string; children: ReactNode }) {
  const on = useContext(MarkdownLinks);
  const files = useContext(MarkdownFiles);
  if (!on) {
    const path = files ? resolvePath(href.replace(/^file:\/\//, ""), files.base) : null;
    if (path && isLocalPath(path) && isFilePath(path))
      return isImagePath(path) ? <InlineImage src={path} alt={label} /> : <FileChip path={path} label={label} />;
    return <>{children}</>;
  }
  return (
    <a href={href} className="md-code-link" title={href} onClick={(e) => { e.preventDefault(); on(href, e); }}>
      {children}
    </a>
  );
}

/**
 * An image in a message: a thumbnail that opens the viewer. In a chat that
 * gathers its images at the end of each turn (`imagesAtEnd`), a local one
 * reads as a card here and shows in full there, once.
 */
function InlineImage({ src: raw, alt }: { src: string; alt: string }) {
  const files = useContext(MarkdownFiles);
  const src = resolvePath(raw, files?.base);
  if (files?.imagesAtEnd && isLocalPath(src)) return <FileChip path={src} label={alt && alt !== raw ? alt : undefined} />;
  return <ImageThumb src={src} alt={alt} />;
}

/** A shown image: a thumbnail that opens it on the stage, or the viewer. */
export function ImageThumb({ src, alt, list }: { src: string; alt?: string; list?: string[] }) {
  const stage = useStage();
  const url = useImageSrc(/^(https?:|data:|blob:)/.test(src) || isLocalPath(src) ? src : null);
  if (!url) return <code>{alt || src}</code>;
  return (
    <button
      className="md-image"
      onClick={() => {
        const all = list?.length ? list : [src];
        const at = Math.max(0, all.indexOf(src));
        if (stage) stage.open(all.map((p) => ({ path: p, name: p === src ? alt || undefined : undefined })), at);
        else openImages(all.map((p) => ({ src: p, name: p === src ? alt || undefined : undefined })), at);
      }}
      title={alt || src}
    >
      <img src={url} alt={alt} />
    </button>
  );
}

/** A file path in backticks: a file card in chats, plain code elsewhere. */
function FileOrCode({ path }: { path: string }) {
  return useContext(MarkdownFiles) ? <FileChip path={path} /> : <code>{path}</code>;
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
    } else if (t.startsWith("`") && isImagePath(t.slice(1, -1)) && !/\s/.test(t.slice(1, -1).trim()) && (isLocalPath(t.slice(1, -1)) || t.includes("/") || t.includes("\\"))) {
      // A screenshot the agent saved, named by path: show it.
      out.push(<InlineImage key={k} src={t.slice(1, -1)} alt={t.slice(1, -1)} />);
    } else if (t.startsWith("`") && isLocalPath(t.slice(1, -1)) && isFilePath(t.slice(1, -1))) {
      // Any other file it names by absolute path (a PDF, a report): a card that opens it.
      out.push(<FileOrCode key={k} path={t.slice(1, -1)} />);
    } else if (t.startsWith("`")) out.push(<code key={k}>{t.slice(1, -1)}</code>);
    else if (t.startsWith("**")) out.push(<strong key={k}>{t.slice(2, -2)}</strong>);
    else if (/^https?:\/\//.test(t)) {
      // A bare link reads as a pill. One tab can summarise (a GitHub PR) shows its title and
      // status; anything else shows owner/repo#12 or host/path.
      if (parseLink(t)?.preview) out.push(<LinkPill key={k} href={t} />);
      else out.push(<a key={k} href={t} className="pr-pill" target="_blank" rel="noreferrer" title={t} onClick={(e) => { e.preventDefault(); void openLink(t, e); }}>{linkLabel(t)}</a>);
    } else if (t.startsWith("[")) {
      const [, label, href] = /\[([^\]]+)\]\(([^)\s]+)\)/.exec(t)!;
      out.push(/^https?:\/\//.test(href) ? <a key={k} href={href} target="_blank" rel="noreferrer" title={href} onClick={(e) => { e.preventDefault(); void openLink(href, e); }}>{label}</a> : <LocalLink key={k} href={href} label={label}>{inline(label, k)}</LocalLink>);
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
