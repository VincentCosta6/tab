// Colors for fenced code in markdown (an agent's reply, a PR body), with the
// same Shiki setup as the review editor (src/modules/github/highlight.ts):
// VS Code's grammars and Dark+/Light+, the JavaScript regex engine (no
// WebAssembly, so the content security policy stays strict), and each
// grammar loaded the first time a block in that language shows.
//
// Spans carry both themes' colors (`--shiki-light`, `--shiki-dark`); CSS
// picks one by the system's color scheme, like the rest of the app.

import type { HighlighterCore, LanguageRegistration } from "shiki/core";

/** Fence name → grammar. Several names share one. */
const GRAMMARS: Record<string, () => Promise<{ default: LanguageRegistration[] }>> = {
  ts: () => import("shiki/langs/tsx.mjs"),
  tsx: () => import("shiki/langs/tsx.mjs"),
  js: () => import("shiki/langs/jsx.mjs"),
  jsx: () => import("shiki/langs/jsx.mjs"),
  json: () => import("shiki/langs/jsonc.mjs"),
  rust: () => import("shiki/langs/rust.mjs"),
  sh: () => import("shiki/langs/shellscript.mjs"),
  css: () => import("shiki/langs/css.mjs"),
  html: () => import("shiki/langs/html.mjs"),
  yaml: () => import("shiki/langs/yaml.mjs"),
  toml: () => import("shiki/langs/toml.mjs"),
  python: () => import("shiki/langs/python.mjs"),
  go: () => import("shiki/langs/go.mjs"),
  diff: () => import("shiki/langs/diff.mjs"),
  sql: () => import("shiki/langs/sql.mjs"),
  markdown: () => import("shiki/langs/markdown.mjs"),
  swift: () => import("shiki/langs/swift.mjs"),
  kotlin: () => import("shiki/langs/kotlin.mjs"),
  proto: () => import("shiki/langs/proto.mjs"),
};

const ALIASES: Record<string, string> = {
  typescript: "ts", javascript: "js", mjs: "js", cjs: "js", jsonc: "json", json5: "json",
  rs: "rust", bash: "sh", zsh: "sh", shell: "sh", shellscript: "sh", console: "sh",
  yml: "yaml", py: "python", golang: "go", patch: "diff", md: "markdown", kt: "kotlin", protobuf: "proto",
};

/** The grammar key for a fence's language, or null when there's none for it. */
export function fenceLanguage(lang: string | undefined): string | null {
  const l = (lang ?? "").toLowerCase();
  const key = ALIASES[l] ?? l;
  return key in GRAMMARS ? key : null;
}

let highlighter: Promise<HighlighterCore> | null = null;
const loaded = new Map<string, Promise<string>>();

function core(): Promise<HighlighterCore> {
  highlighter ??= (async () => {
    const [{ createHighlighterCore }, { createJavaScriptRegexEngine }, dark, light] = await Promise.all([
      import("shiki/core"),
      import("shiki/engine/javascript"),
      import("shiki/themes/dark-plus.mjs"),
      import("shiki/themes/light-plus.mjs"),
    ]);
    return createHighlighterCore({
      themes: [light.default, dark.default],
      langs: [],
      // Skip the odd pattern JavaScript's regex engine can't express.
      engine: createJavaScriptRegexEngine({ forgiving: true }),
    });
  })();
  return highlighter;
}

/** Loads a grammar once; resolves to the name Shiki knows it by. */
function language(h: HighlighterCore, key: string): Promise<string> {
  let p = loaded.get(key);
  if (!p) {
    p = GRAMMARS[key]().then(async (m) => {
      await h.loadLanguage(...m.default);
      return m.default[m.default.length - 1].name;
    });
    loaded.set(key, p);
  }
  return p;
}

/**
 * Highlighted HTML for `code` (Shiki escapes the text), or null when the
 * language has no grammar here or highlighting fails; the block then shows
 * as plain text.
 */
export async function highlightCode(code: string, lang: string | undefined): Promise<string | null> {
  const key = fenceLanguage(lang);
  if (!key) return null;
  try {
    const h = await core();
    const name = await language(h, key);
    return h.codeToHtml(code, { lang: name, themes: { light: "light-plus", dark: "dark-plus" }, defaultColor: false });
  } catch (e) {
    console.warn("[tab] code highlighting", e);
    return null;
  }
}
