// A slash command's argument hint (`[low|high] [--fix] [<pr#>|<branch>]`),
// read into slots for the composer: what's still to type shows as ghost
// text, and each slot's known values are offered in the `/` menu.

/** Where a placeholder's values come from. */
export type ArgSource = "pr" | "branch" | "path" | "model" | "agent";

/** One argument the command takes: the hint's text for it, the literal values it offers, and placeholders by source. */
export interface ArgSlot { text: string; values: string[]; sources: ArgSource[]; free: boolean }

/** Splits on `sep` outside `[…]` and `<…>`. */
function splitTop(s: string, sep: (c: string) => boolean): string[] {
  const out: string[] = [];
  let depth = 0;
  let cur = "";
  for (const c of s) {
    if (c === "[" || c === "<") depth++;
    else if ((c === "]" || c === ">") && depth > 0) depth--;
    if (depth === 0 && sep(c)) {
      if (cur.trim()) out.push(cur.trim());
      cur = "";
    } else cur += c;
  }
  if (cur.trim()) out.push(cur.trim());
  return out;
}

/** A slot without its outer `[…]` or `<…>`, when that pair encloses all of it (not `<a> <b>`). */
function unwrap(s: string): { inner: string; bracketed: boolean } {
  if (!/^[[<]/.test(s) || !/[\]>]$/.test(s)) return { inner: s, bracketed: false };
  let depth = 0;
  for (let i = 0; i < s.length - 1; i++) {
    if (s[i] === "[" || s[i] === "<") depth++;
    else if (s[i] === "]" || s[i] === ">") depth--;
    if (depth === 0) return { inner: s, bracketed: false };
  }
  return { inner: s.slice(1, -1).trim(), bracketed: true };
}

/** Which tab-side values a placeholder name stands for, if any. */
export function sourceFor(name: string): ArgSource | null {
  const n = name.toLowerCase().replace(/[<>[\]#\s_-]+/g, "");
  if (["pr", "prnumber", "pull", "pullrequest", "prnum"].includes(n)) return "pr";
  if (["branch", "ref", "basebranch"].includes(n)) return "branch";
  if (["path", "file", "files", "dir", "folder", "directory", "target"].includes(n)) return "path";
  if (n === "model") return "model";
  if (["agent", "subagent", "agenttype"].includes(n)) return "agent";
  return null;
}

const literal = /^[\w.#:@+=-]+$/;

/** Reads an argument hint into slots. */
export function parseHint(hint: string): ArgSlot[] {
  // `consent | revoke` reads as one slot.
  const norm = hint.trim().replace(/\s*\|\s*/g, "|");
  return splitTop(norm, (c) => /\s/.test(c)).map((text) => {
    const { inner } = unwrap(text);
    const alts = splitTop(inner, (c) => c === "|");
    const slot: ArgSlot = { text, values: [], sources: [], free: false };
    // `--max-findings <n>|all`: offer the flag; what follows it is its value.
    const flag = alts.map((a) => /^(--?[\w-]+)\s/.exec(a)?.[1]).find(Boolean);
    if (flag) return { ...slot, values: [flag] };
    for (const a of alts) {
      const { inner: name, bracketed: named } = unwrap(a);
      // A lone word is a name for what goes there (`[name]`, `key=value`),
      // unless it's a flag; among alternatives, words are the values.
      const lone = alts.length === 1 && !a.startsWith("-");
      // `disable [<server>|all]` among alternatives: the word is the value.
      const head = /^([\w.-]+)\s+[[<]/.exec(a)?.[1];
      if (!named && !lone && literal.test(a)) slot.values.push(a);
      else if (head && alts.length > 1) slot.values.push(head);
      else if (named && name.includes("|")) slot.values.push(...parseHint(name).flatMap((s) => s.values));
      else {
        const src = sourceFor(name);
        if (src) slot.sources.push(src);
        else slot.free = true;
      }
    }
    return slot;
  });
}

/** Whether `arg` could fill `slot`. */
function fits(slot: ArgSlot, arg: string) {
  return slot.values.includes(arg) || slot.sources.length > 0 || slot.free;
}

/**
 * The slots still open after the arguments typed so far: each argument
 * fills the first open slot naming it, or else the first open slot that
 * takes a placeholder. Optional slots can be skipped, so order is a guide.
 */
export function openSlots(slots: ArgSlot[], args: string[]): ArgSlot[] {
  const open = [...slots];
  for (const a of args) {
    let i = open.findIndex((s) => s.values.includes(a));
    if (i < 0) i = open.findIndex((s) => fits(s, a) && !s.values.length);
    if (i < 0) i = open.findIndex((s) => fits(s, a));
    if (i >= 0) open.splice(i, 1);
  }
  return open;
}

/** What's typed after the command: whole arguments, and the word being typed (empty after a space). */
export function splitArgs(rest: string): { args: string[]; word: string } {
  const parts = rest.split(/\s+/);
  const word = parts.pop() ?? "";
  return { args: parts.filter(Boolean), word };
}
