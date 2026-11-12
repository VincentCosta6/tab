// Fails if a package the app ships (pnpm's production dependencies) has a
// license outside the list in deny.toml, so the JavaScript side follows the
// same rule as the Rust side. Run with `pnpm licenses:check`.
//
// An SPDX expression passes when it can be satisfied with allowed licenses:
// every part of an AND, at least one part of an OR.
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";

const deny = readFileSync(new URL("../../deny.toml", import.meta.url), "utf8");
const block = deny.match(/\[licenses\][\s\S]*?allow\s*=\s*\[([\s\S]*?)\]/);
if (!block) throw new Error("no [licenses] allow list in deny.toml");
const allowed = new Set([...block[1].matchAll(/"([^"]+)"/g)].map((m) => m[1]));

/** Whether an SPDX expression can be met with allowed licenses. */
export function satisfied(expr) {
  const tokens = expr.replace(/\(/g, " ( ").replace(/\)/g, " ) ").split(/\s+/).filter(Boolean);
  let i = 0;
  const primary = () => {
    if (tokens[i] === "(") {
      i++;
      const v = or();
      i++; // ")"
      return v;
    }
    let id = tokens[i++];
    if (tokens[i] === "WITH") id += ` WITH ${tokens[(i += 2) - 1]}`;
    return allowed.has(id);
  };
  const and = () => {
    let v = primary();
    while (tokens[i] === "AND") (i++, (v = primary() && v));
    return v;
  };
  const or = () => {
    let v = and();
    while (tokens[i] === "OR") (i++, (v = and() || v));
    return v;
  };
  return or();
}

const out = execFileSync("pnpm", ["licenses", "list", "--prod", "--json"], {
  encoding: "utf8",
  shell: process.platform === "win32",
});
const bad = [];
for (const [license, pkgs] of Object.entries(JSON.parse(out))) {
  if (!satisfied(license)) bad.push(...pkgs.map((p) => `${p.name}@${p.versions?.join(",")}: ${license}`));
}
if (bad.length) {
  console.error("Packages with a license tab doesn't allow (see deny.toml and CLAUDE.md):");
  for (const b of bad) console.error(`  ${b}`);
  process.exit(1);
}
console.log("JavaScript licenses ok");
