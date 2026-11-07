#!/usr/bin/env bash
# The full storage test suite. Too slow for every CI run, so run it when you
# touch tab-store (agents: after adding or changing a storage test, run this
# and make sure it passes), and on a cadence.
#
#   scripts/storage-tests.sh          conformance + property tests + hub
#   scripts/storage-tests.sh --perf   also the 100k-item volume/timing tests
#   scripts/storage-tests.sh --cov    also line coverage of tab-store
#                                     (needs cargo-llvm-cov; target 95%+)
set -euo pipefail
cd "$(dirname "$0")/.."

echo "== conformance + property tests (every backend)"
cargo test -p tab-store --all-features
echo "== hub (runs the protocol over every backend, migration both ways)"
cargo test -p tab-hub

for arg in "$@"; do
  case "$arg" in
    --perf)
      echo "== volume and timing (release)"
      cargo test -p tab-store --all-features --release --test perf -- --ignored --nocapture
      ;;
    --cov)
      echo "== coverage"
      # The suite itself is excluded so it can't pad the number.
      cargo llvm-cov -p tab-store --all-features --ignore-filename-regex 'conformance\.rs|tests/' --fail-under-lines 95 --summary-only
      ;;
    *) echo "unknown option $arg" >&2; exit 2 ;;
  esac
done
