// Full markdown for agent replies, finished or still streaming: GitHub
// flavored (tables, numbered and task lists, strikethrough), code with
// colors and a copy button, and half-written markdown (an open code fence,
// `**bol`) shown as it will end up rather than as stray symbols.
//
// Streamdown parses and repairs the stream; every element is tab's own
// (Streamdown's are styled for Tailwind, which tab doesn't use), so links,
// file cards and images behave as in `Markdown`.

import { isValidElement, memo, useEffect, useState, type CSSProperties, type ReactNode } from "react";
import { Streamdown, defaultRehypePlugins, useIsCodeFenceIncomplete, type Components } from "streamdown";
import type { BundledLanguage } from "shiki";
import { createCodePlugin, type HighlightResult } from "@streamdown/code";
import { Icon } from "./icons";
import { BareLink, InlineCode, InlineImage, LocalLink, WebLink } from "./markdown";

/** Plain text of rendered children (a link's label, a code span). */
function textOf(n: ReactNode): string {
  if (n == null || typeof n === "boolean") return "";
  if (typeof n === "string" || typeof n === "number") return String(n);
  if (Array.isArray(n)) return n.map(textOf).join("");
  if (isValidElement<{ children?: ReactNode }>(n)) return textOf(n.props.children);
  return "";
}

function Link({ href, children }: { href?: string; children?: ReactNode }) {
  if (!href) return <>{children}</>;
  if (/^https?:\/\//.test(href)) {
    // A bare URL (GFM autolinks it, so its label is the URL) reads as a pill.
    const label = textOf(children);
    return label === href || label === href.replace(/^https?:\/\//, "") ? <BareLink href={href} /> : <WebLink href={href}>{children}</WebLink>;
  }
  if (href.startsWith("mailto:")) return <a href={href}>{children}</a>;
  return <LocalLink href={href} label={textOf(children)}>{children}</LocalLink>;
}

// Streamdown's Shiki plugin: every bundled language, loaded on first use,
// with the JavaScript regex engine (no WebAssembly, so the content security
// policy stays strict). A block still streaming in is highlighted from its
// last complete line on rather than from the top each token.
const highlighter = createCodePlugin({ themes: ["light-plus", "dark-plus"] });

function useHighlighted(code: string, lang: string | undefined): HighlightResult | null {
  const streaming = useIsCodeFenceIncomplete();
  const [result, setResult] = useState<HighlightResult | null>(null);
  useEffect(() => {
    const language = (lang ?? "").toLowerCase() as BundledLanguage;
    if (!language || !highlighter.supportsLanguage(language)) return setResult(null);
    let live = true;
    const now = highlighter.highlight(
      { code, language, themes: highlighter.getThemes(), isIncomplete: streaming },
      (r) => live && setResult(r),
    );
    if (now) setResult(now);
    return () => { live = false; };
  }, [code, lang, streaming]);
  return result;
}

function CodeBlock({ code, lang }: { code: string; lang?: string }) {
  const highlighted = useHighlighted(code, lang);
  const [copied, setCopied] = useState(false);
  const copy = () => {
    void navigator.clipboard.writeText(code).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 1200);
    });
  };
  return (
    <div className="md-code">
      <div className="md-code-head">
        <span>{lang || "text"}</span>
        <button className="md-code-copy" onClick={copy} title="Copy" aria-label="Copy code">
          <Icon name={copied ? "check" : "copy"} size={12} />
        </button>
      </div>
      <pre className="md-code-body">
        <code>
          {highlighted
            ? highlighted.tokens.map((line, i) => (
                <span key={i} className="line">
                  {/* Each token carries its light color and `--shiki-dark`. */}
                  {line.map((t, j) => <span key={j} style={t.htmlStyle as CSSProperties}>{t.content}</span>)}
                  {i < highlighted.tokens.length - 1 && "\n"}
                </span>
              ))
            : code}
        </code>
      </pre>
    </div>
  );
}

const components: Components = {
  a: ({ href, children }) => <Link href={href}>{children}</Link>,
  img: ({ src, alt }) => (typeof src === "string" && src ? <InlineImage src={src} alt={alt ?? ""} /> : null),
  inlineCode: ({ children }) => <InlineCode text={textOf(children)} />,
  code: ({ children }) => <InlineCode text={textOf(children)} />,
  // A fenced block is <pre><code class="language-x">: render it whole here.
  pre: ({ children }) => {
    const code = Array.isArray(children) ? children.find(isValidElement) : children;
    const props = isValidElement<{ className?: string; children?: ReactNode }>(code) ? code.props : { children };
    const lang = /language-([^\s]+)/.exec(props.className ?? "")?.[1];
    return <CodeBlock code={textOf(props.children).replace(/\n$/, "")} lang={lang} />;
  },
  table: ({ children }) => <div className="md-table"><table>{children}</table></div>,
  thead: ({ children }) => <thead>{children}</thead>,
  tbody: ({ children }) => <tbody>{children}</tbody>,
  tr: ({ children }) => <tr>{children}</tr>,
  th: ({ children, style }) => <th style={style}>{children}</th>,
  td: ({ children, style }) => <td style={style}>{children}</td>,
  p: ({ children }) => <p>{children}</p>,
  ul: ({ children, className }) => <ul className={className}>{children}</ul>,
  ol: ({ children, start }) => <ol start={start}>{children}</ol>,
  li: ({ children, className }) => <li className={className}>{children}</li>,
  blockquote: ({ children }) => <blockquote>{children}</blockquote>,
  hr: () => <hr />,
  strong: ({ children }) => <strong>{children}</strong>,
  em: ({ children }) => <em>{children}</em>,
  del: ({ children }) => <del>{children}</del>,
  h1: ({ children }) => <h3>{children}</h3>,
  h2: ({ children }) => <h4>{children}</h4>,
  h3: ({ children }) => <h5>{children}</h5>,
  h4: ({ children }) => <h6>{children}</h6>,
  h5: ({ children }) => <h6>{children}</h6>,
  h6: ({ children }) => <h6>{children}</h6>,
};

// Raw HTML is parsed and then sanitized, which drops unsafe URLs such as
// `javascript:`. Streamdown's last step, rehype-harden, is left out: it
// blocks every relative link, and agents link files that way
// (`[isRetryable](src/capture.ts#L23)`); the components above decide what
// a link does.
const rehypePlugins = [defaultRehypePlugins.raw, defaultRehypePlugins.sanitize];

/** An agent's reply as markdown. `streaming` while it's still being written. */
export const RichMarkdown = memo(function RichMarkdown({ text, streaming = false }: { text: string; streaming?: boolean }) {
  return (
    <Streamdown
      className="markdown rich-markdown"
      mode={streaming ? "streaming" : "static"}
      isAnimating={streaming}
      parseIncompleteMarkdown={streaming}
      components={components}
      rehypePlugins={rehypePlugins}
      controls={false}
      linkSafety={{ enabled: false }}
    >
      {text}
    </Streamdown>
  );
});
