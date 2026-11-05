// Bundles Monaco locally (no CDN), so the review tool works offline and
// inside the desktop app's strict webview.

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

export function languageFor(path: string): string {
  const ext = path.split(".").pop()?.toLowerCase() ?? "";
  const map: Record<string, string> = {
    ts: "typescript", tsx: "typescript", js: "javascript", jsx: "javascript", mjs: "javascript", cjs: "javascript",
    rs: "rust", go: "go", py: "python", rb: "ruby", java: "java", kt: "kotlin", swift: "swift", cs: "csharp",
    json: "json", md: "markdown", css: "css", scss: "scss", html: "html", yml: "yaml", yaml: "yaml", toml: "ini",
    sql: "sql", sh: "shell", c: "c", h: "c", cpp: "cpp", hpp: "cpp", php: "php",
  };
  return map[ext] ?? "plaintext";
}

export { monaco };
