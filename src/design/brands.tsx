// Real product marks for sources. Simple Icons provides most; Slack's mark
// isn't distributed there, so it is drawn here from its four-color logo.
// Very dark marks (GitHub, OpenCode, Notion) follow the text color so they
// stay visible in dark mode.

import { siClaude, siFigma, siGithub, siGmail, siGooglecalendar, siJira, siLinear, siNotion, siOpencode, siZoom } from "simple-icons";
import type { Tone } from "../lib/types";
import { Icon } from "./icons";

type SimpleIcon = { path: string; hex: string; title: string };

const simple: Record<string, SimpleIcon> = {
  github: siGithub,
  jira: siJira,
  zoom: siZoom,
  gmail: siGmail,
  claude: siClaude,
  opencode: siOpencode,
  linear: siLinear,
  notion: siNotion,
  figma: siFigma,
  calendar: siGooglecalendar,
};

const dark = new Set(["github", "opencode", "notion"]);

function SlackMark({ size }: { size: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 124 124" aria-label="Slack">
      <path d="M26.4 78.2c0 7.1-5.8 12.9-12.9 12.9S.6 85.3.6 78.2c0-7.1 5.8-12.9 12.9-12.9h12.9v12.9z" fill="#E01E5A" />
      <path d="M32.9 78.2c0-7.1 5.8-12.9 12.9-12.9s12.9 5.8 12.9 12.9v32.3c0 7.1-5.8 12.9-12.9 12.9s-12.9-5.8-12.9-12.9V78.2z" fill="#E01E5A" />
      <path d="M45.8 26.4c-7.1 0-12.9-5.8-12.9-12.9S38.7.6 45.8.6s12.9 5.8 12.9 12.9v12.9H45.8z" fill="#36C5F0" />
      <path d="M45.8 32.9c7.1 0 12.9 5.8 12.9 12.9s-5.8 12.9-12.9 12.9H13.5C6.4 58.7.6 52.9.6 45.8s5.8-12.9 12.9-12.9h32.3z" fill="#36C5F0" />
      <path d="M97.6 45.8c0-7.1 5.8-12.9 12.9-12.9s12.9 5.8 12.9 12.9-5.8 12.9-12.9 12.9H97.6V45.8z" fill="#2EB67D" />
      <path d="M91.1 45.8c0 7.1-5.8 12.9-12.9 12.9s-12.9-5.8-12.9-12.9V13.5C65.3 6.4 71.1.6 78.2.6s12.9 5.8 12.9 12.9v32.3z" fill="#2EB67D" />
      <path d="M78.2 97.6c7.1 0 12.9 5.8 12.9 12.9s-5.8 12.9-12.9 12.9-12.9-5.8-12.9-12.9V97.6h12.9z" fill="#ECB22E" />
      <path d="M78.2 91.1c-7.1 0-12.9-5.8-12.9-12.9s5.8-12.9 12.9-12.9h32.3c7.1 0 12.9 5.8 12.9 12.9s-5.8 12.9-12.9 12.9H78.2z" fill="#ECB22E" />
    </svg>
  );
}

/** Zoom's app tile (Simple Icons only has the wordmark, which is unreadable this small). */
function ZoomMark({ size }: { size: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" aria-label="Zoom">
      <rect width="24" height="24" rx="6" fill="#0B5CFF" />
      <path d="M5 8.6c0-.6.5-1.1 1.1-1.1h6.6c1.3 0 2.4 1.1 2.4 2.4v4.9c0 .6-.5 1.1-1.1 1.1H7.4A2.4 2.4 0 0 1 5 13.5zM16.1 10.6l2.4-1.9c.4-.3.9 0 .9.5v5.6c0 .5-.5.8-.9.5l-2.4-1.9z" fill="#fff" />
    </svg>
  );
}

/** tab's own mark: a folder-style tab over a stack. */
export function TabMark({ size = 20 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" aria-label="tab">
      <path d="M3 7.5A2.5 2.5 0 0 1 5.5 5h4.2l2 2.5h6.8A2.5 2.5 0 0 1 21 10v7.5a2.5 2.5 0 0 1-2.5 2.5h-13A2.5 2.5 0 0 1 3 17.5z" fill="var(--brand-tab, #4f46e5)" />
      <path d="M7 12.5h10M7 15.5h6" stroke="#fff" strokeWidth="1.8" strokeLinecap="round" />
    </svg>
  );
}

export const brandNames = ["slack", "tab", ...Object.keys(simple)];

export function hasBrand(name: string) {
  return name === "slack" || name === "tab" || name in simple;
}

export function Brand({ name, size = 16 }: { name: string; size?: number }) {
  if (name === "slack") return <SlackMark size={size} />;
  if (name === "zoom") return <ZoomMark size={size} />;
  if (name === "tab") return <TabMark size={size} />;
  const icon = simple[name];
  if (!icon) return <Icon name={name} size={size} />;
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" role="img" aria-label={icon.title}>
      <path d={icon.path} fill={dark.has(name) ? "currentColor" : `#${icon.hex}`} />
    </svg>
  );
}

/** Which product an item comes from, by its kind prefix or source. */
export function brandFor(kind: string, source?: string): string | null {
  const app = kind.split(".")[0];
  const byApp: Record<string, string> = { slack: "slack", github: "github", email: "gmail", meeting: "zoom", jira: "jira", tab: "tab", linear: "linear" };
  return byApp[app] ?? (source && hasBrand(source) ? source : null);
}

/**
 * The square glyph at the start of a row: the product's mark on a soft tile,
 * with a small badge for what happened (review, CI failure, mention...).
 */
export function SourceGlyph({ kind, source, icon, tone = "neutral", size = 28 }: { kind: string; source?: string; icon: string; tone?: Tone; size?: number }) {
  const brand = brandFor(kind, source);
  const showBadge = brand && icon !== brand && !hasBrand(icon);
  return (
    <span className={`source-glyph ${brand ? "branded" : `tone-${tone}`}`} style={{ width: size, height: size }}>
      {brand ? <Brand name={brand} size={Math.round(size * 0.58)} /> : <Icon name={icon} size={Math.round(size * 0.55)} />}
      {showBadge && (
        <span className={`glyph-badge tone-${tone}`}>
          <Icon name={icon} size={9} />
        </span>
      )}
    </span>
  );
}
