#!/usr/bin/env node
// `pnpm dev:tab`: a dev build of tab that runs beside the installed app.
//
// - Its own identity (src-tauri/tauri.dev.conf.json): app id dev.tab.dev, so
//   its own tab.db, hub.db and config.json, plus its own ports.
// - UI changes hot-reload through Vite. The backend keeps running, so agent
//   chats carry on untouched.
// - Rust changes rebuild in the background while the old build keeps
//   running. The app restarts onto the new build only once no agent turn is
//   running, so a save never cuts an agent off mid-turn. Press r to restart
//   now anyway: tab resumes the interrupted turns when it comes back.
//
// Keys: r restart now, b rebuild now, q quit.
// Flags: --copy-config  start from the installed app's config.json
//                        (connections and tokens), if the dev build has none.

import { spawn, spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const isWin = process.platform === "win32";
const devConf = JSON.parse(
  fs.readFileSync(path.join(root, "src-tauri", "tauri.dev.conf.json"), "utf8"),
);
const identifier = devConf.identifier;
const devUrl = new URL(devConf.build.devUrl);
const targetDir = process.env.CARGO_TARGET_DIR
  ? path.resolve(process.env.CARGO_TARGET_DIR)
  : path.join(root, "target");
const exe = isWin ? "tab.exe" : "tab";
const built = path.join(targetDir, "debug", exe);
const runDir = path.join(targetDir, "dev-run");
const statusFile = path.join(runDir, "status.json");

// Rust sources: a change here needs a rebuild. Everything under src/ is
// Vite's and hot-reloads.
const WATCH = [
  "crates",
  "src-tauri/src",
  "src-tauri/capabilities",
  "src-tauri/build.rs",
  "src-tauri/Cargo.toml",
  "src-tauri/tauri.conf.json",
  "src-tauri/tauri.dev.conf.json",
  "src-tauri/Info.plist",
  "Cargo.toml",
  "Cargo.lock",
];

const say = (msg) => process.stdout.write(`\x1b[35m[tab dev]\x1b[0m ${msg}\n`);

// --- Folders --------------------------------------------------------------

/** Tauri's app_config_dir for an identifier. */
function configDir(id) {
  const home = os.homedir();
  if (process.platform === "darwin") return path.join(home, "Library", "Application Support", id);
  if (isWin) return path.join(process.env.APPDATA ?? path.join(home, "AppData", "Roaming"), id);
  return path.join(process.env.XDG_CONFIG_HOME ?? path.join(home, ".config"), id);
}

function copyConfig() {
  const from = path.join(configDir("dev.tab.app"), "config.json");
  const to = path.join(configDir(identifier), "config.json");
  if (fs.existsSync(to)) {
    say(`dev config already exists at ${to}; leaving it`);
    return;
  }
  if (!fs.existsSync(from)) {
    say(`no installed config at ${from}; starting fresh`);
    return;
  }
  fs.mkdirSync(path.dirname(to), { recursive: true });
  fs.copyFileSync(from, to);
  say(`copied the installed app's config to ${to}`);
}

// --- Vite -------------------------------------------------------------------

function startVite() {
  const vite = spawn(
    "pnpm",
    ["exec", "vite", "--port", devUrl.port, "--strictPort"],
    { cwd: root, stdio: ["ignore", "inherit", "inherit"], shell: isWin },
  );
  vite.on("exit", (code) => {
    if (!quitting) {
      say(`vite exited (${code}); stopping`);
      quit(1);
    }
  });
  return vite;
}

async function waitForVite() {
  for (let i = 0; i < 120; i++) {
    try {
      await fetch(devUrl);
      return;
    } catch {
      await sleep(250);
    }
  }
  throw new Error(`vite didn't come up on ${devUrl}`);
}

// --- Building ---------------------------------------------------------------

let building = null;
let buildAgain = false;
/** A build that succeeded and isn't running yet. */
let staged = null;

function build() {
  if (building) {
    buildAgain = true;
    return building;
  }
  building = new Promise((resolve) => {
    say("building the backend…");
    const started = Date.now();
    const cargo = spawn("cargo", ["build", "-p", "tab"], {
      cwd: root,
      stdio: "inherit",
      // Merged into tauri.conf.json at compile time, as `tauri dev --config` does.
      env: { ...process.env, TAURI_CONFIG: JSON.stringify(devConf) },
      shell: isWin,
    });
    cargo.on("exit", (code) => {
      if (code === 0) {
        // Run a copy, so the next build can replace the binary while this
        // one runs (Windows won't let it otherwise).
        fs.mkdirSync(runDir, { recursive: true });
        const copy = path.join(runDir, `tab-dev-${Date.now()}${isWin ? ".exe" : ""}`);
        fs.copyFileSync(built, copy);
        if (!isWin) fs.chmodSync(copy, 0o755);
        staged = copy;
        say(`built in ${((Date.now() - started) / 1000).toFixed(0)}s`);
      } else {
        say("build failed; the last good build keeps running");
      }
      resolve(code === 0);
    });
  }).then(async (ok) => {
    building = null;
    if (buildAgain) {
      buildAgain = false;
      return build();
    }
    return ok;
  });
  return building;
}

function cleanOldCopies(keep) {
  for (const f of fs.readdirSync(runDir, { withFileTypes: true })) {
    if (f.isFile() && f.name.startsWith("tab-dev-") && !keep.includes(path.join(runDir, f.name))) {
      try {
        fs.unlinkSync(path.join(runDir, f.name));
      } catch {
        // Still running, or Windows still holds it; next time.
      }
    }
  }
}

// --- The app ------------------------------------------------------------------

let app = null;
let running = null;
let restarting = false;

function launch() {
  if (!staged) return;
  running = staged;
  staged = null;
  try {
    fs.unlinkSync(statusFile);
  } catch {}
  say("starting tab dev");
  app = spawn(running, [], {
    cwd: path.join(root, "src-tauri"),
    stdio: "inherit",
    env: { ...process.env, TAB_DEV_STATUS: statusFile },
  });
  const me = app;
  me.on("exit", (code, signal) => {
    if (app !== me) return;
    app = null;
    if (!restarting && !quitting) {
      say(`tab dev exited (${signal ?? code}); stopping`);
      quit(0);
    }
  });
  cleanOldCopies([running]);
}

/** Agent turns running in the app now, or 0 if it hasn't said yet. */
function working() {
  try {
    const s = JSON.parse(fs.readFileSync(statusFile, "utf8"));
    return app && s.pid === app.pid ? s.working : 0;
  } catch {
    return 0;
  }
}

/** Ends the app and everything it started (agents, language servers). */
async function stopApp() {
  const proc = app;
  if (!proc) return;
  const gone = new Promise((r) => proc.once("exit", r));
  if (isWin) {
    // Agents sit in job objects that close with the app; /T gets the rest.
    spawnSync("taskkill", ["/PID", String(proc.pid), "/T", "/F"], { stdio: "ignore" });
  } else {
    // Agents lead their own process groups, so they don't go with the app
    // on their own: end each child's group first.
    const kids = spawnSync("pgrep", ["-P", String(proc.pid)], { encoding: "utf8" });
    for (const pid of (kids.stdout ?? "").split(/\s+/).filter(Boolean).map(Number)) {
      try {
        process.kill(-pid, "SIGKILL");
      } catch {
        try {
          process.kill(pid, "SIGKILL");
        } catch {}
      }
    }
    try {
      proc.kill("SIGKILL");
    } catch {}
  }
  await gone;
}

async function restart() {
  if (!staged) return;
  restarting = true;
  await stopApp();
  launch();
  restarting = false;
}

let waitingSince = null;

/** Restarts onto a staged build as soon as no agent is working. */
async function restartWhenIdle() {
  if (!staged || restarting) return;
  if (!app) return launch();
  const n = working();
  if (n === 0) {
    waitingSince = null;
    say("restarting onto the new build");
    return restart();
  }
  if (!waitingSince) {
    waitingSince = Date.now();
    say(
      `new build ready; waiting for ${n} agent turn${n === 1 ? "" : "s"} to finish before restarting ` +
        `(press r to restart now; tab resumes cut-off turns)`,
    );
  }
}

// --- Watching -----------------------------------------------------------------

let debounce = null;

function watch() {
  const skip = (f) =>
    !f ||
    /(^|[\\/])(target|node_modules|gen)([\\/]|$)/.test(f) ||
    /(~|\.sw[a-p]|\.tmp)$/.test(f) ||
    path.basename(f) === "4913";
  for (const rel of WATCH) {
    const p = path.join(root, rel);
    if (!fs.existsSync(p)) continue;
    const recursive = fs.statSync(p).isDirectory();
    fs.watch(p, { recursive }, (_event, file) => {
      if (recursive && skip(file)) return;
      clearTimeout(debounce);
      debounce = setTimeout(() => {
        say(`changed: ${recursive ? path.join(rel, file) : rel}`);
        build().then(() => restartWhenIdle());
      }, 400);
    });
  }
}

// --- Keys and quitting ----------------------------------------------------------

let quitting = false;
let vite = null;
let quitWarned = false;

async function quit(code) {
  if (quitting) return;
  quitting = true;
  clearInterval(poll);
  await stopApp();
  if (vite && vite.exitCode === null) {
    if (isWin) spawnSync("taskkill", ["/PID", String(vite.pid), "/T", "/F"], { stdio: "ignore" });
    else vite.kill("SIGTERM");
  }
  process.exit(code);
}

function askQuit() {
  const n = working();
  if (n > 0 && !quitWarned) {
    quitWarned = true;
    say(`${n} agent turn${n === 1 ? " is" : "s are"} running. Quit again to stop anyway (tab resumes them next start).`);
    setTimeout(() => (quitWarned = false), 5000);
    return;
  }
  quit(0);
}

function keys() {
  process.on("SIGINT", askQuit);
  process.on("SIGTERM", () => quit(0));
  if (!process.stdin.isTTY) return;
  process.stdin.setRawMode(true);
  process.stdin.setEncoding("utf8");
  process.stdin.on("data", async (k) => {
    if (k === "\u0003" || k === "q") return askQuit();
    if (k === "b") return build().then(() => restartWhenIdle());
    if (k === "r") {
      if (!staged && !(await build())) return;
      waitingSince = null;
      say("restarting now");
      return restart();
    }
  });
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
let poll = null;

async function main() {
  if (process.argv.includes("--copy-config")) copyConfig();
  vite = startVite();
  keys();
  const [ok] = await Promise.all([build(), waitForVite()]);
  if (!ok) {
    say("the first build failed; fix it and save to try again");
  }
  launch();
  watch();
  poll = setInterval(restartWhenIdle, 1000);
  say(`UI at ${devUrl} hot-reloads. Keys: r restart now, b rebuild, q quit.`);
}

main().catch((e) => {
  console.error(e);
  quit(1);
});
