# Working in tab

See `docs/ARCHITECTURE.md` for the crate map. Standing rules:

## Storage layer (`crates/tab-store`)

- Every behavior of the storage layer has a test, and every backend passes all of them. Add or change a `HubStore` behavior only together with a check in `crates/tab-store/src/conformance.rs`, registered in the `conformance_tests!` list (a test fails if you forget).
- A storage bug gets a failing conformance check first, then the fix.
- A new backend runs `conformance_tests!` against itself and joins the differential property tests in `crates/tab-store/tests/properties.rs`.
- These suites don't run in CI (too slow). After adding or changing a storage test or backend, run `scripts/storage-tests.sh` and make sure it passes; add `--perf` if you touched query shapes or indexes, and `--cov` to check coverage stays at 95%+.

## Shared crates

- `tab-domain`, `tab-protocol`, `tab-store` (without `sqlite`) and `tab-hub` must build for `wasm32-unknown-unknown`: no I/O, threads, tokio or clock reads. Pass `now` in. CI checks this.
- `hub.proto` changes are additive only. Never renumber or reuse a field; reserve removed ones. Run `pnpm proto` to regenerate the TypeScript and commit it.

## The desktop app

- Don't edit the live checkout the app runs from; work in a git worktree. Agents editing the running checkout have crashed the app before.

## tab cloud and `tab-infra`

The cloud side (edge Worker, hub Durable Object, accounts in D1) is the private `VincentCosta6/tab-infra` repo. The two repos share one API contract, and it lives here:

- `crates/tab-protocol/proto/tab/cloud/v1/account.proto` is the account API (`/v1/hub`, `/v1/hubs`, …). tab-infra builds its responses from the `tab-protocol` crate, pinned by git `rev` in its `Cargo.toml`. `tab_core::cloud`, the `cloud_*` Tauri commands and `src/lib/protocol/cloud.ts` are the app's side.
- Additive only, like `hub.proto`: never renumber, reuse or change the type of a field; reserve removed ones. Released apps keep talking to the newest Worker, so a field the app needs must be optional to the old app and the Worker must still answer old apps.
- A change to the API, or to `tab-protocol`, `tab-hub` or `tab-store` that the cloud runs, is two changes: merge it here first, then bump the `rev` in tab-infra and merge there. Nothing reaches production until tab-infra's `main` deploys it. Say so in the PR, and link the tab-infra PR.
- Run `pnpm proto` after editing the proto and commit the generated TypeScript.

## Third-party code

tab is FSL-1.1-MIT; everything it ships must allow that. `deny.toml` holds the allowed licenses and sources, and CI checks every Rust crate (`cargo deny check licenses bans sources`) and npm package (`pnpm licenses:check`) against it.

- **Depend, never copy.** Don't paste code from another project, not even one function: no tool can see copied code, so its license goes unchecked. Add the crate or package instead, and let the checks read its license.
- **Zed.** Only Zed's Apache-2.0 crates, and only from `https://github.com/zed-industries/zed` pinned to a full commit `rev` (never a branch), with `default-features = false` where the crate allows it. The one exception is the GPUI app (`crates/tab-gpui`): it takes GPUI as the `gpui-pre*` crates.io snapshots that `gpui-component` (Apache-2.0) builds on, pinned with `=` to the exact versions `gpui-component` requires. Bump `gpui-pre`, `gpui-pre-platform` and `gpui-component` together, and check each snapshot crate's license is still Apache-2.0. Zed's GPL-3.0 crates are off limits, as dependencies and as code to copy or translate: `editor`, `multi_buffer`, `buffer_diff`, `rope`, `text`, `language`, `lsp`, `terminal`, `markdown`, `ui`, `project`, `workspace`, `extension_host`, and anything else whose `Cargo.toml` says GPL or AGPL. Check a Zed crate's `license` field before adding it; its siblings can differ. Zed's `extension_api` may be read as a design reference only.
- **Adding a license to `deny.toml`** needs a reason in a comment there. Copyleft (GPL, AGPL, LGPL) never goes on the list. Keep `about.toml`'s list in step.
- **Never `private = { ignore = true }`** in `deny.toml`: Zed marks its crates `publish = false`, so that would skip them. tab's own crates are listed as exceptions by name instead.
- **ACP adapters** (claude-agent-acp, codex-acp, pi-acp) run as separate programs and don't touch tab's code, but each `acp.adapter` entry in `harnesses.json` records its license, and a test refuses one outside the list before tab offers to install it.
- Releases ship `THIRD_PARTY_LICENSES.txt` (`pnpm licenses:notices`, needs `cargo-about`), shown in Settings → Updates. A weekly workflow (`zed-license-watch.yml`) opens an issue if Zed relicenses a crate tab pins.
