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

const PROJECT_EXT = /\.(tsx?|mts|cts|jsx?|mjs|cjs)$/i;
const PROJECT_JSON = /(^|\/)(package\.json|tsconfig[^/]*\.json|jsconfig\.json)$/i;
const SKIP = /(^|\/)(node_modules|dist|build|out|target|vendor|\.next|coverage|gen)\//;

/** Files worth loading so TypeScript can resolve imports across the repo. */
export function projectFiles(files: string[], limit = 3000) {
  return files.filter((f) => (PROJECT_EXT.test(f) || PROJECT_JSON.test(f)) && !SKIP.test(f) && !/\.min\.js$/.test(f)).slice(0, limit);
}

export interface ProjectState { key: string; done: number; total: number; state: "loading" | "ready" | "error"; error?: string }

// One loader for the whole app, so remounting the workspace (switching tabs,
// reopening the PR) picks up the load already running instead of waiting on it.
let project: ProjectState | null = null;
let running: Promise<void> | null = null;
const listeners = new Set<() => void>();
const loaded = new Set<string>();
const set = (p: ProjectState | null) => {
  project = p;
  listeners.forEach((l) => l());
};
export const projectStore = {
  get: () => project,
  subscribe: (l: () => void) => {
    listeners.add(l);
    return () => listeners.delete(l);
  },
};

const BATCH = 80;
const MAX_BYTES = 40 * 1024 * 1024;

/** Loads a repository's source files as models, in small batches, yielding to the UI between them. */
export function loadProject(key: string, files: string[], read: (paths: string[]) => Promise<[string, string][]>): Promise<void> {
  if (project?.key === key && running) return running;
  if (project?.key === key && project.state === "ready") return Promise.resolve();
  const want = projectFiles(files);
  if (!want.length) {
    set(null);
    return Promise.resolve();
  }
  // Drop the previous project's models, except ones an editor still shows.
  const shown = new Set(monaco.editor.getEditors().map((e) => e.getModel()?.uri.toString()));
  for (const uri of loaded) {
    const m = monaco.editor.getModel(monaco.Uri.parse(uri));
    if (m && !shown.has(uri)) m.dispose();
  }
  loaded.clear();
  set({ key, done: 0, total: want.length, state: "loading" });
  const job = (async () => {
    let done = 0;
    let bytes = 0;
    let failures = 0;
    for (let i = 0; i < want.length && bytes < MAX_BYTES; i += BATCH) {
      if (project?.key !== key) return;
      let batch: [string, string][] = [];
      try {
        batch = await read(want.slice(i, i + BATCH));
      } catch (e) {
        // A bad batch shouldn't stop the rest; give up only if most fail.
        if (++failures > 3) throw e;
      }
      if (project?.key !== key) return;
      for (const [path, text] of batch) {
        const uri = headUri(path);
        // Leave models an editor already created (they may hold newer text).
        if (!monaco.editor.getModel(uri)) monaco.editor.createModel(text, languageFor(path), uri);
        loaded.add(uri.toString());
        bytes += text.length;
      }
      done = Math.min(want.length, i + BATCH);
      set({ key, done, total: want.length, state: "loading" });
      await new Promise((r) => setTimeout(r, 0));
    }
    set({ key, done: want.length, total: want.length, state: "ready" });
  })()
    .catch((e) => {
      if (project?.key === key) set({ key, done: project.done, total: project.total, state: "error", error: String(e) });
    })
    .finally(() => {
      if (project?.key === key) running = null;
    });
  running = job;
  return job;
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
