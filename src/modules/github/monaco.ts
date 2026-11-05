// Bundles Monaco locally (no CDN), so the review tool works offline and
// inside the desktop app's strict webview. Also owns the "project": every
// head-side file is a model at file:///<path>, so the TypeScript service sees
// the whole repository and go to definition, hover, references and peek work
// across files. Base-side files live under base:///<path>.

import * as monaco from "monaco-editor";
import { loader } from "@monaco-editor/react";
import EditorWorker from "monaco-editor/editor/editor.worker?worker";
import TsWorker from "monaco-editor/language/typescript/ts.worker?worker";
import JsonWorker from "monaco-editor/language/json/json.worker?worker";
import CssWorker from "monaco-editor/language/css/css.worker?worker";

(self as unknown as { MonacoEnvironment: monaco.Environment }).MonacoEnvironment = {
  getWorker(_id: string, label: string) {
    if (label === "typescript" || label === "javascript") return new TsWorker();
    if (label === "json") return new JsonWorker();
    if (label === "css" || label === "scss" || label === "less") return new CssWorker();
    return new EditorWorker();
  },
};

loader.config({ monaco });

// Repositories rarely come with node_modules here, so semantic errors would
// be mostly noise. Syntax errors still show, and every language feature works.
for (const d of [monaco.typescript.typescriptDefaults, monaco.typescript.javascriptDefaults]) {
  d.setCompilerOptions({
    target: monaco.typescript.ScriptTarget.ESNext,
    module: monaco.typescript.ModuleKind.ESNext,
    moduleResolution: monaco.typescript.ModuleResolutionKind.NodeJs,
    jsx: monaco.typescript.JsxEmit.ReactJSX,
    allowJs: true,
    checkJs: false,
    allowNonTsExtensions: true,
    allowSyntheticDefaultImports: true,
    esModuleInterop: true,
    resolveJsonModule: true,
    skipLibCheck: true,
    noEmit: true,
    baseUrl: "file:///",
  });
  d.setDiagnosticsOptions({ noSemanticValidation: true, noSyntaxValidation: false, noSuggestionDiagnostics: true });
  d.setEagerModelSync(true);
}

export function languageFor(path: string): string {
  const name = path.split("/").pop()?.toLowerCase() ?? "";
  if (name === "dockerfile") return "dockerfile";
  if (name === "makefile") return "makefile";
  const ext = name.includes(".") ? name.split(".").pop()! : "";
  const map: Record<string, string> = {
    ts: "typescript", tsx: "typescript", mts: "typescript", cts: "typescript", js: "javascript", jsx: "javascript", mjs: "javascript", cjs: "javascript",
    rs: "rust", go: "go", py: "python", rb: "ruby", java: "java", kt: "kotlin", kts: "kotlin", swift: "swift", cs: "csharp", fs: "fsharp",
    json: "json", jsonc: "json", md: "markdown", mdx: "markdown", css: "css", scss: "scss", less: "less", html: "html", vue: "html", svelte: "html",
    yml: "yaml", yaml: "yaml", toml: "ini", ini: "ini", sql: "sql", sh: "shell", bash: "shell", zsh: "shell", ps1: "powershell",
    c: "c", h: "c", cc: "cpp", cpp: "cpp", hpp: "cpp", m: "objective-c", php: "php", lua: "lua", dart: "dart", scala: "scala",
    graphql: "graphql", gql: "graphql", proto: "protobuf", xml: "xml", svg: "xml", r: "r", ex: "elixir", exs: "elixir", clj: "clojure",
  };
  return map[ext] ?? "plaintext";
}

export const headUri = (path: string) => monaco.Uri.from({ scheme: "file", path: `/${path}` });
export const baseUri = (path: string) => monaco.Uri.from({ scheme: "base", path: `/${path}` });
export const pathOf = (uri: monaco.Uri) => uri.path.replace(/^\//, "");

/** Creates or updates the model for a file. Models are shared by every editor. */
export function ensureModel(uri: monaco.Uri, text: string, path: string) {
  const existing = monaco.editor.getModel(uri);
  if (existing) {
    if (existing.getValue() !== text) existing.setValue(text);
    return existing;
  }
  return monaco.editor.createModel(text, languageFor(path), uri);
}

const PROJECT_EXT = /\.(tsx?|mts|cts|jsx?|mjs|cjs|json)$/i;
const SKIP = /(^|\/)(node_modules|dist|build|out|target|vendor|\.next|coverage)\//;

/** Files worth loading so TypeScript can resolve imports across the repo. */
export function projectFiles(files: string[], limit = 4000) {
  return files.filter((f) => PROJECT_EXT.test(f) && !SKIP.test(f) && !/\.min\.js$/.test(f) && !/(^|\/)package-lock\.json$/.test(f)).slice(0, limit);
}

let projectKey = "";
/** Loads a repository's source files as models, in batches. Returns how many loaded. */
export async function loadProject(key: string, files: string[], read: (paths: string[]) => Promise<[string, string][]>, onProgress?: (done: number, total: number) => void) {
  if (projectKey === key) return 0;
  projectKey = key;
  // Drop the previous project's head models; base models are cheap to recreate.
  for (const m of monaco.editor.getModels()) if (m.uri.scheme === "file" || m.uri.scheme === "base") m.dispose();
  const want = projectFiles(files);
  let done = 0;
  for (let i = 0; i < want.length; i += 250) {
    if (projectKey !== key) return done;
    const batch = await read(want.slice(i, i + 250));
    if (projectKey !== key) return done;
    for (const [path, text] of batch) ensureModel(headUri(path), text, path);
    done += batch.length;
    onProgress?.(done, want.length);
  }
  return done;
}

type Opener = (path: string, line?: number, column?: number) => void;
let opener: Opener | null = null;
/** Go to definition into another file opens it as a tab in the workspace. */
export function setOpener(f: Opener | null) {
  opener = f;
}
monaco.editor.registerEditorOpener({
  openCodeEditor(_source, resource, sel) {
    if (!opener || resource.scheme !== "file") return false;
    const pos = sel && "startLineNumber" in sel ? { lineNumber: sel.startLineNumber, column: sel.startColumn } : (sel as monaco.IPosition | undefined);
    opener(pathOf(resource), pos?.lineNumber, pos?.column);
    return true;
  },
});

export { monaco };
