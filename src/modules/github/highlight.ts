// VS Code's own TextMate grammars and Dark+/Light+ colors for Monaco, via
// Shiki. Monaco's built-in tokenizers are simple regexes: JSX, generics,
// decorators and template strings come out flat. These grammars are the ones
// VS Code ships, so code reads the same here as in the editor.
//
// Shiki runs with its JavaScript regex engine (no WebAssembly, so the
// desktop app's content security policy stays strict) and loads its grammars
// lazily, the first time a review editor opens.
//
// Language servers add semantic tokens on top (types, parameters, readonly
// variables); `SEMANTIC` colors them the way VS Code's themes do.

import type { LanguageRegistration, ThemeRegistrationRaw } from "shiki/core";
import { monaco } from "./monaco";

export type ThemeName = "tab-light" | "tab-dark";

let current: ThemeName = "tab-light";
let ready: Promise<void> | null = null;

/** Switches every editor's theme. Use this, not `monaco.editor.setTheme`. */
export function applyTheme(name: ThemeName) {
  current = name;
  monaco.editor.setTheme(name);
}

/** Semantic token colors, from VS Code's Dark+ and Light+. */
const SEMANTIC: Record<ThemeName, Record<string, string>> = {
  "tab-dark": {
    type: "#4EC9B0", class: "#4EC9B0", interface: "#4EC9B0", enum: "#4EC9B0", struct: "#4EC9B0", typeParameter: "#4EC9B0", namespace: "#4EC9B0",
    function: "#DCDCAA", method: "#DCDCAA", decorator: "#DCDCAA", macro: "#DCDCAA",
    parameter: "#9CDCFE", variable: "#9CDCFE", property: "#9CDCFE",
    enumMember: "#4FC1FF", "variable.readonly": "#4FC1FF", "variable.declaration.readonly": "#4FC1FF", "property.readonly": "#4FC1FF",
  },
  "tab-light": {
    type: "#267F99", class: "#267F99", interface: "#267F99", enum: "#267F99", struct: "#267F99", typeParameter: "#267F99", namespace: "#267F99",
    function: "#795E26", method: "#795E26", decorator: "#795E26", macro: "#795E26",
    parameter: "#001080", variable: "#001080", property: "#001080",
    enumMember: "#0070C1", "variable.readonly": "#0070C1", "variable.declaration.readonly": "#0070C1", "property.readonly": "#0070C1",
  },
};

/** Monaco language id → Shiki grammar loader. */
const GRAMMARS: Record<string, () => Promise<{ default: LanguageRegistration[] }>> = {
  // .ts and .tsx share Monaco's "typescript" id, so they share the TSX
  // grammar, which VS Code also uses for TypeScript React and which reads
  // plain TypeScript fine (generic arrows included).
  typescript: () => import("shiki/langs/tsx.mjs"),
  // VS Code's JavaScript grammar already understands JSX.
  javascript: () => import("shiki/langs/javascript.mjs"),
  json: () => import("shiki/langs/jsonc.mjs"),
  css: () => import("shiki/langs/css.mjs"),
  scss: () => import("shiki/langs/scss.mjs"),
  less: () => import("shiki/langs/less.mjs"),
  html: () => import("shiki/langs/html.mjs"),
  markdown: () => import("shiki/langs/markdown.mjs"),
  yaml: () => import("shiki/langs/yaml.mjs"),
  ini: () => import("shiki/langs/toml.mjs"),
  rust: () => import("shiki/langs/rust.mjs"),
  go: () => import("shiki/langs/go.mjs"),
  python: () => import("shiki/langs/python.mjs"),
  sql: () => import("shiki/langs/sql.mjs"),
  shell: () => import("shiki/langs/shellscript.mjs"),
  powershell: () => import("shiki/langs/powershell.mjs"),
  dockerfile: () => import("shiki/langs/docker.mjs"),
  makefile: () => import("shiki/langs/make.mjs"),
  java: () => import("shiki/langs/java.mjs"),
  kotlin: () => import("shiki/langs/kotlin.mjs"),
  swift: () => import("shiki/langs/swift.mjs"),
  csharp: () => import("shiki/langs/csharp.mjs"),
  cpp: () => import("shiki/langs/cpp.mjs"),
  c: () => import("shiki/langs/c.mjs"),
  ruby: () => import("shiki/langs/ruby.mjs"),
  php: () => import("shiki/langs/php.mjs"),
  lua: () => import("shiki/langs/lua.mjs"),
  graphql: () => import("shiki/langs/graphql.mjs"),
  xml: () => import("shiki/langs/xml.mjs"),
  protobuf: () => import("shiki/langs/proto.mjs"),
};

/** Shiki's grammar for `id` renamed to the Monaco language id it serves. */
async function grammar(id: string, load: () => Promise<{ default: LanguageRegistration[] }>) {
  const langs = (await load()).default;
  // The last registration is the language itself; earlier ones are its embeds.
  const main = langs[langs.length - 1];
  return [...langs.slice(0, -1), { ...main, name: id, aliases: [] } as LanguageRegistration];
}

function theme(name: ThemeName, base: ThemeRegistrationRaw, colors: Record<string, string>): ThemeRegistrationRaw {
  const semantic = Object.entries(SEMANTIC[name]).map(([scope, foreground]) => ({ scope, settings: { foreground } }));
  return {
    ...base,
    name,
    colors: { ...(base.colors ?? {}), ...colors },
    tokenColors: [...(base.tokenColors ?? base.settings ?? []), ...semantic],
    settings: undefined as unknown as ThemeRegistrationRaw["settings"],
  };
}

/**
 * Loads the grammars and themes once and hands Monaco's tokenizers over to
 * them. Editors keep working (with Monaco's own colors) until it's done.
 */
export function setupHighlighting(colors: Record<ThemeName, Record<string, string>>): Promise<void> {
  if (ready) return ready;
  ready = (async () => {
    const [{ createHighlighterCore }, { createJavaScriptRegexEngine }, { shikiToMonaco }, dark, light] = await Promise.all([
      import("shiki/core"),
      import("shiki/engine/javascript"),
      import("@shikijs/monaco"),
      import("shiki/themes/dark-plus.mjs"),
      import("shiki/themes/light-plus.mjs"),
    ]);
    const known = new Set(monaco.languages.getLanguages().map((l) => l.id));
    const ids = Object.keys(GRAMMARS).filter((id) => known.has(id));
    const langs = (await Promise.all(ids.map((id) => grammar(id, GRAMMARS[id]).catch(() => [])))).flat();
    const highlighter = await createHighlighterCore({
      themes: [theme("tab-light", light.default as ThemeRegistrationRaw, colors["tab-light"]), theme("tab-dark", dark.default as ThemeRegistrationRaw, colors["tab-dark"])],
      langs,
      // Skip the odd pattern JavaScript's regex engine can't express.
      engine: createJavaScriptRegexEngine({ forgiving: true }),
    });
    shikiToMonaco(highlighter, monaco);
    // shikiToMonaco switches to its first theme; put the app's back.
    monaco.editor.setTheme(current);
  })().catch((e) => {
    // Monaco's own highlighting stays; nothing else depends on this.
    console.warn("[tab] highlighting", e);
  });
  return ready;
}
