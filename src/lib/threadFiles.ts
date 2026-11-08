// The files in a chat: what the person attached and what the agent named
// (screenshots, reports, a file it wrote). The file stage steps through
// them, the context rail lists them and the tray above the composer pins
// them, so a file doesn't mean scrolling back up the chat to find it.

import { useMemo } from "react";
import { fileName, isFilePath, isImagePath, isLocalPath, resolvePath } from "./images";
import type { Chat, SessionMessage } from "./types";

export interface ThreadFile {
  path: string;
  name: string;
  /** Who brought it into the chat. */
  from: "you" | "agent";
  /** An attachment the agent replaced with its own version: the file it was. */
  original?: string;
}

const WRITE_TOOLS = new Set(["Write", "write", "Edit", "edit", "MultiEdit", "NotebookEdit", "apply_patch"]);
const toolName = (t: string) => t.replace(/^mcp__[^_]+(?:_[^_]+)*?__/, "").split(":")[0];

/** Local files an agent's reply names, in the order it names them, as its markdown shows them. */
export function filesInText(text: string, base: string | null): string[] {
  const out: string[] = [];
  const blocks = text.replace(/\r\n/g, "\n").split(/\n{2,}/).filter((b) => !b.startsWith("```"));
  const re = /!\[[^\]]*\]\(([^)\s]+)\)|`([^`]+)`|\[[^\]]+\]\(([^)\s]+)\)/g;
  for (const b of blocks) {
    let m: RegExpExecArray | null;
    while ((m = re.exec(b))) {
      const [, img, code, href] = m;
      if (img) {
        const p = resolvePath(img, base);
        if (isLocalPath(p)) out.push(p);
      } else if (code) {
        const t = code.trim();
        if (isImagePath(t) && !/\s/.test(t) && (isLocalPath(t) || /[\\/]/.test(t))) {
          const p = resolvePath(t, base);
          if (isLocalPath(p)) out.push(p);
        } else if (isLocalPath(t) && isFilePath(t)) out.push(t);
      } else if (href && !/^https?:/.test(href)) {
        const p = resolvePath(href.replace(/^file:\/\//, ""), base);
        if (isLocalPath(p) && isFilePath(p)) out.push(p);
      }
    }
  }
  return out;
}

/**
 * The chat's files, oldest first, each once. When the agent writes a file
 * with the same name as one the person attached, its version takes the
 * attachment's place (keeping `original`), so the file on show follows it.
 */
export function threadFiles(messages: SessionMessage[], base: string | null): ThreadFile[] {
  const list: ThreadFile[] = [];
  const byPath = new Map<string, ThreadFile>();
  const attached = new Map<string, ThreadFile>(); // by lowercased name
  const add = (path: string, from: ThreadFile["from"]) => {
    if (byPath.has(path)) return;
    const name = fileName(path);
    const earlier = from === "agent" ? attached.get(name.toLowerCase()) : undefined;
    if (earlier) {
      // The agent's version of an attachment: it stands in for it.
      earlier.original ??= earlier.path;
      earlier.path = path;
      byPath.set(path, earlier);
      return;
    }
    const f: ThreadFile = { path, name, from };
    list.push(f);
    byPath.set(path, f);
    if (from === "you") attached.set(name.toLowerCase(), f);
  };
  for (const m of messages) {
    if (m.role === "user") {
      const paths: unknown = m.meta?.attachments;
      if (Array.isArray(paths)) for (const p of paths) if (typeof p === "string") add(p, "you");
    } else if (m.kind === "text" && m.role === "assistant") {
      for (const p of filesInText(m.content, base)) add(p, "agent");
    } else if (m.kind === "tool" && WRITE_TOOLS.has(toolName(m.meta?.tool ?? ""))) {
      // A write to a file named like an attachment replaces it, even before the reply names it.
      const input = m.meta?.input ?? {};
      const raw = input.file_path ?? input.filePath ?? input.notebook_path;
      if (typeof raw !== "string") continue;
      const p = resolvePath(raw, base);
      if (attached.has(fileName(p).toLowerCase())) add(p, "agent");
    }
  }
  return list;
}

export function useThreadFiles(chat: Chat | undefined): ThreadFile[] {
  const messages = chat?.messages;
  const base = chat?.session?.cwd ?? null;
  return useMemo(() => threadFiles(messages ?? [], base), [messages, base]);
}
