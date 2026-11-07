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
