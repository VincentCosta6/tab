// Tiny, safe markdown: paragraphs, bullet lists, headings, `code`, **bold**,
// *italic* and links. No HTML passthrough. Enough for PR bodies and comments
// until we need a real renderer.

import { createContext, useContext, type MouseEvent, type ReactNode } from "react";
import { openLink } from "../lib/links";

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

function inline(text: string, key: string): ReactNode[] {
  const out: ReactNode[] = [];
  const re = /(`[^`]+`|\*\*[^*]+\*\*|\*[^*]+\*|\[[^\]]+\]\([^)\s]+\))/g;
  let last = 0;
  let m: RegExpExecArray | null;
  let i = 0;
  while ((m = re.exec(text))) {
    if (m.index > last) out.push(text.slice(last, m.index));
    const t = m[0];
    const k = `${key}-${i++}`;
    if (t.startsWith("`")) out.push(<code key={k}>{t.slice(1, -1)}</code>);
    else if (t.startsWith("**")) out.push(<strong key={k}>{t.slice(2, -2)}</strong>);
    else if (t.startsWith("[")) {
      const [, label, href] = /\[([^\]]+)\]\(([^)\s]+)\)/.exec(t)!;
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
