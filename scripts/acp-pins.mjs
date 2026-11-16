#!/usr/bin/env node
// `node scripts/acp-pins.mjs`: moves the ACP adapters tab installs (see
// crates/tab-core/src/agents/adapter.rs) to the versions in the ACP registry
// (github.com/agentclientprotocol/registry), and Node to the newest LTS.
//
// - Release builds: the registry's archive URL and sha256 for each target,
//   into harnesses.json.
// - npm adapters: a fresh lockfile for the registry's version, in
//   crates/tab-core/src/agents/adapters/<command>.lock.json. Refuses one
//   where any package runs an install script.
// - Node: the newest LTS and the sha256s from nodejs.org's SHASUMS256.txt.
//
// Review the diff like any dependency update before committing it.

import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const agents = path.join(root, "crates/tab-core/src/agents");
const harnessesPath = path.join(agents, "harnesses.json");
const REGISTRY = "https://raw.githubusercontent.com/agentclientprotocol/registry/main";
// The registry's id for each adapter, by its command.
const REGISTRY_ID = { "claude-agent-acp": "claude-acp", "codex-acp": "codex-acp" };
// The registry's targets, as tab names them.
const TARGET = {
  "darwin-aarch64": "darwin-arm64",
  "darwin-x86_64": "darwin-x64",
  "linux-aarch64": "linux-arm64",
  "linux-x86_64": "linux-x64",
  "linux-aarch64-musl": "linux-arm64-musl",
  "linux-x86_64-musl": "linux-x64-musl",
  "windows-aarch64": "windows-arm64",
  "windows-x86_64": "windows-x64",
};
// Node's archives, by tab's target.
const NODE = {
  "darwin-arm64": "darwin-arm64.tar.gz",
  "darwin-x64": "darwin-x64.tar.gz",
  "linux-arm64": "linux-arm64.tar.gz",
  "linux-x64": "linux-x64.tar.gz",
  "windows-arm64": "win-arm64.zip",
  "windows-x64": "win-x64.zip",
};

async function get(url, as = "json") {
  const r = await fetch(url);
  if (!r.ok) throw new Error(`${url}: ${r.status}`);
  return as === "json" ? r.json() : r.text();
}

const sorted = (o) => Object.fromEntries(Object.entries(o).sort(([a], [b]) => a.localeCompare(b)));

const harnesses = JSON.parse(fs.readFileSync(harnessesPath, "utf8"));
for (const h of harnesses) {
  const cmd = h.acp?.command;
  const adapter = h.acp?.adapter;
  if (!cmd || !adapter) continue;
  const id = REGISTRY_ID[cmd];
  if (!id) {
    console.warn(`${cmd}: not in the registry map, left as is`);
    continue;
  }
  const entry = await get(`${REGISTRY}/${id}/agent.json`);
  const dist = entry.distribution ?? {};
  if (adapter.release) {
    if (!dist.binary) throw new Error(`${cmd}: the registry lists no binaries`);
    const builds = {};
    for (const [t, b] of Object.entries(dist.binary)) {
      if (!TARGET[t]) throw new Error(`${cmd}: unknown target ${t}`);
      if (!/^[0-9a-f]{64}$/.test(b.sha256 ?? "")) throw new Error(`${cmd} ${t}: no sha256`);
      builds[TARGET[t]] = { url: b.archive, sha256: b.sha256 };
    }
    adapter.release.version = entry.version;
    adapter.release.builds = sorted(builds);
  } else if (adapter.npm) {
    const spec = dist.npx?.package ?? "";
    const version = spec.slice(spec.lastIndexOf("@") + 1);
    if (spec !== `${adapter.npm}@${version}` || !version) {
      throw new Error(`${cmd}: the registry's package is ${spec}, not ${adapter.npm}`);
    }
    adapter.version = version;
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), "tab-acp-pins-"));
    fs.writeFileSync(
      path.join(dir, "package.json"),
      JSON.stringify({ name: "tab-acp-adapter", private: true, dependencies: { [adapter.npm]: version } }, null, 2),
    );
    const r = spawnSync("npm", ["install", "--package-lock-only", "--ignore-scripts", "--no-audit", "--no-fund"], {
      cwd: dir,
      stdio: "inherit",
      shell: process.platform === "win32",
    });
    if (r.status !== 0) throw new Error(`${cmd}: npm couldn't lock ${spec}`);
    const lock = JSON.parse(fs.readFileSync(path.join(dir, "package-lock.json"), "utf8"));
    const scripts = Object.entries(lock.packages).filter(([, p]) => p.hasInstallScript);
    if (scripts.length) throw new Error(`${cmd}: install scripts in ${scripts.map(([k]) => k).join(", ")}`);
    fs.writeFileSync(path.join(agents, "adapters", `${cmd}.lock.json`), JSON.stringify(lock, null, 2) + "\n");
    fs.rmSync(dir, { recursive: true, force: true });
  }
  console.log(`${cmd}: ${entry.version}`);
}
fs.writeFileSync(harnessesPath, JSON.stringify(harnesses, null, 2) + "\n");

const lts = (await get("https://nodejs.org/dist/index.json")).find((r) => r.lts);
const sums = await get(`https://nodejs.org/dist/${lts.version}/SHASUMS256.txt`, "text");
const builds = {};
for (const [t, suffix] of Object.entries(NODE)) {
  const file = `node-${lts.version}-${suffix}`;
  const line = sums.split("\n").find((l) => l.endsWith(`  ${file}`));
  if (!line) throw new Error(`node ${lts.version}: no ${file}`);
  builds[t] = { url: `https://nodejs.org/dist/${lts.version}/${file}`, sha256: line.split(/\s+/)[0] };
}
fs.writeFileSync(
  path.join(agents, "adapters", "node.json"),
  JSON.stringify({ version: lts.version.slice(1), builds }, null, 2) + "\n",
);
console.log(`node: ${lts.version}`);
