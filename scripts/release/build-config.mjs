// Writes the Tauri config overlay a release build uses, merged over
// src-tauri/tauri.conf.json with `tauri build --config <file>`.
//
//   node scripts/release/build-config.mjs <out.json>
//
// Reads only the environment, so the same release builds with or without
// signing set up:
//   VERSION                        the release version, like 1.2.3 (required)
//   APPLE_CERTIFICATE              set: Developer ID signing. Unset: ad-hoc
//                                  signature, so Gatekeeper says "unidentified
//                                  developer" instead of "damaged".
//   TAURI_SIGNING_PRIVATE_KEY      set: also build signed updater bundles
//   TAURI_UPDATER_PUBKEY           set: the app checks for updates, verifying
//                                  them with this key (the private key's
//                                  public half). Unset: it can't update itself.
//   AZURE_SIGNING_ENDPOINT, AZURE_SIGNING_ACCOUNT, AZURE_SIGNING_PROFILE
//                                  all set: sign Windows binaries with Azure
//                                  Artifact Signing (needs artifact-signing-cli
//                                  and AZURE_CLIENT_ID/SECRET/TENANT_ID)
import { writeFileSync } from "node:fs";

const env = (name) => (process.env[name] ?? "").trim();
const out = process.argv[2];
if (!out) {
  console.error("usage: build-config.mjs <out.json>");
  process.exit(2);
}
const version = env("VERSION");
if (!/^\d+\.\d+\.\d+$/.test(version)) {
  console.error(`VERSION must be X.Y.Z, got "${version}"`);
  process.exit(1);
}

const config = { version, bundle: {} };
const notes = [`version ${version}`];

if (env("TAURI_SIGNING_PRIVATE_KEY")) {
  config.bundle.createUpdaterArtifacts = true;
  notes.push("updater bundles: signed");
} else {
  notes.push("updater bundles: off (no TAURI_SIGNING_PRIVATE_KEY)");
}

const pubkey = env("TAURI_UPDATER_PUBKEY");
if (pubkey) {
  config.plugins = { updater: { pubkey } };
  notes.push("in-app updates: on");
  if (!env("TAURI_SIGNING_PRIVATE_KEY")) {
    console.log("::warning::TAURI_UPDATER_PUBKEY is set but TAURI_SIGNING_PRIVATE_KEY isn't, so this release has no update bundles to offer.");
  }
} else {
  notes.push("in-app updates: off (no TAURI_UPDATER_PUBKEY)");
  if (env("TAURI_SIGNING_PRIVATE_KEY")) {
    console.log("::warning::TAURI_SIGNING_PRIVATE_KEY is set but TAURI_UPDATER_PUBKEY isn't, so this build can't check for updates.");
  }
}

if (process.platform === "darwin") {
  if (env("APPLE_CERTIFICATE")) {
    notes.push("macOS: Developer ID signing");
  } else {
    config.bundle.macOS = { signingIdentity: "-" };
    notes.push("macOS: ad-hoc signature (no APPLE_CERTIFICATE)");
  }
}

if (process.platform === "win32") {
  const endpoint = env("AZURE_SIGNING_ENDPOINT");
  const account = env("AZURE_SIGNING_ACCOUNT");
  const profile = env("AZURE_SIGNING_PROFILE");
  if (endpoint && account && profile) {
    config.bundle.windows = {
      signCommand: `artifact-signing-cli -e ${endpoint} -a ${account} -c ${profile} -d tab %1`,
    };
    notes.push("Windows: Azure Artifact Signing");
  } else {
    notes.push("Windows: unsigned (no Azure Artifact Signing variables)");
  }
}

writeFileSync(out, JSON.stringify(config, null, 2));
console.log(notes.join("\n"));
