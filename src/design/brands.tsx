// Real product marks for sources. Simple Icons provides most; Slack's mark
// isn't distributed there, so it is drawn here from its four-color logo.
// Very dark marks (GitHub, OpenCode, Notion) follow the text color so they
// stay visible in dark mode.

import { siApple, siClaude, siFigma, siGithub, siGmail, siGooglecalendar, siJira, siLinear, siNotion, siOpencode, siZoom } from "simple-icons";
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
  apple: siApple,
};

const dark = new Set(["github", "opencode", "notion", "apple"]);

/** Google's four-colour "G", as on its sign-in buttons (Simple Icons only has it in one colour). */
function GoogleMark({ size }: { size: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 48 48" aria-label="Google">
      <path fill="#EA4335" d="M24 9.5c3.54 0 6.71 1.22 9.21 3.6l6.85-6.85C35.9 2.38 30.47 0 24 0 14.62 0 6.51 5.38 2.56 13.22l7.98 6.19C12.43 13.72 17.74 9.5 24 9.5z" />
      <path fill="#4285F4" d="M46.98 24.55c0-1.57-.15-3.09-.38-4.55H24v9.02h12.94c-.58 2.96-2.26 5.48-4.78 7.18l7.73 6c4.51-4.18 7.09-10.36 7.09-17.65z" />
      <path fill="#FBBC05" d="M10.53 28.59c-.48-1.45-.76-2.99-.76-4.59s.27-3.14.76-4.59l-7.98-6.19C.92 16.46 0 20.12 0 24c0 3.88.92 7.54 2.56 10.78l7.97-6.19z" />
      <path fill="#34A853" d="M24 48c6.48 0 11.93-2.13 15.89-5.81l-7.73-6c-2.15 1.45-4.92 2.3-8.16 2.3-6.26 0-11.57-4.22-13.47-9.91l-7.98 6.19C6.51 42.62 14.62 48 24 48z" />
    </svg>
  );
}

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

/** tab's own mark, as the app icon: three lines merging into one. */
export function TabMark({ size = 20 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 1024 1024" aria-label="tab">
      <rect width="1024" height="1024" rx="230" fill="#1c1c20" />
      <g fill="none" strokeLinecap="round" strokeWidth="84">
        <path d="M220 300 C 420 300, 440 512, 600 512" stroke="#7dd3fc" />
        <path d="M220 724 C 420 724, 440 512, 600 512" stroke="#f9a8d4" />
        <path d="M220 512 H 600" stroke="#fcd34d" />
      </g>
      <rect x="580" y="452" width="230" height="120" rx="60" fill="#fff" />
    </svg>
  );
}

export const brandNames = ["slack", "tab", ...Object.keys(simple)];

export function hasBrand(name: string) {
  return name === "slack" || name === "tab" || name === "google" || name in simple;
}

export function Brand({ name, size = 16 }: { name: string; size?: number }) {
  if (name === "slack") return <SlackMark size={size} />;
  if (name === "google") return <GoogleMark size={size} />;
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
