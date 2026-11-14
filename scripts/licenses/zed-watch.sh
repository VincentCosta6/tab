#!/bin/sh
# Weekly upstream watch (.github/workflows/zed-license-watch.yml): for each
# crate tab takes from Zed's repository (pinned by `rev` in Cargo.lock),
# compares the `license` field at the pinned commit with Zed's main branch.
# Zed could relicense a crate; the pinned copy keeps its license, but the
# next bump wouldn't. Prints one line per change and exits 1 if any.
#
# Needs `gh` signed in (GH_TOKEN in Actions).
set -eu
cd "$(dirname "$0")/../.."
repo=zed-industries/zed
status="${TMPDIR:-/tmp}/zed-watch.$$"
echo 0 > "$status"
# `name` then `source` lines of each Zed package in Cargo.lock.
pkgs=$(awk '
  /^name = / { gsub(/"/, "", $3); name = $3 }
  /^source = "git\+https:\/\/github.com\/zed-industries\/zed/ {
    split($0, a, "#"); rev = a[2]; gsub(/"/, "", rev); print name, rev
  }' Cargo.lock | sort -u)
[ -n "$pkgs" ] || { echo "no crates from $repo"; exit 0; }
license_at() { # crate ref
  gh api "repos/$repo/contents/crates/$1/Cargo.toml?ref=$2" --jq .content 2>/dev/null \
    | base64 -d 2>/dev/null \
    | sed -n 's/^license *= *"\(.*\)"/\1/p' | head -1
}
echo "$pkgs" | while read -r name rev; do
  pinned=$(license_at "$name" "$rev")
  latest=$(license_at "$name" main)
  if [ "$pinned" != "$latest" ]; then
    echo "$name: ${pinned:-none} at ${rev} -> ${latest:-missing} on main"
    echo 1 > "$status"
  else
    echo "ok $name: $pinned" >&2
  fi
done
code=$(cat "$status")
rm -f "$status"
exit "$code"
