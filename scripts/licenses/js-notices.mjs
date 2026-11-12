// The npm half of THIRD_PARTY_LICENSES.txt: each production package's
// license file and NOTICE, as pnpm installed them.
import { execFileSync } from "node:child_process";
import { existsSync, readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";

const out = execFileSync("pnpm", ["licenses", "list", "--prod", "--json"], {
  encoding: "utf8",
  shell: process.platform === "win32",
});
const pkgs = Object.values(JSON.parse(out)).flat().sort((a, b) => a.name.localeCompare(b.name));
const lines = ["", "=== npm packages ==="];
for (const p of pkgs) {
  lines.push("", "-".repeat(72), `${p.name} ${p.versions.join(", ")} (${p.license})`, "-".repeat(72), "");
  const dir = p.paths?.[0];
  const files = dir && existsSync(dir) ? readdirSync(dir).filter((f) => /^(licen[cs]e|copying|notice)/i.test(f)).sort() : [];
  if (!files.length) lines.push(`${p.license}; see ${p.homepage ?? "the package"}.`);
  for (const f of files) lines.push(readFileSync(join(dir, f), "utf8").trim(), "");
}
console.log(lines.join("\n"));
