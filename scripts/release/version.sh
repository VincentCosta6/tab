#!/usr/bin/env bash
# Release version checks, used by .github/workflows/release.yml and runnable
# by hand.
#
#   scripts/release/version.sh check v1.2.3
#       The tag must be strict vMAJOR.MINOR.PATCH (no leading zeros, no
#       pre-release or build suffix). Prints 1.2.3.
#
#   scripts/release/version.sh unique v1.2.3 < existing-tags
#       Fails if the tag is already in the list (one tag per line: git tags,
#       release tags, drafts included). Warns, without failing, when it isn't
#       the highest version so far: a hotfix on an older line is allowed,
#       but the updater only offers versions newer than the one installed.
set -euo pipefail

SEMVER='^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$'

die() { echo "error: $*" >&2; exit 1; }

check() {
  local tag="${1:-}"
  [[ "$tag" =~ $SEMVER ]] || die "\"$tag\" isn't a release version. Use vMAJOR.MINOR.PATCH, like v1.2.3 (no leading zeros or suffixes)."
  # MSI (WiX) caps major and minor at 255 and patch at 65535.
  (( BASH_REMATCH[1] <= 255 && BASH_REMATCH[2] <= 255 && BASH_REMATCH[3] <= 65535 )) \
    || die "\"$tag\" is too large for a Windows installer (major and minor up to 255, patch up to 65535)."
  echo "${tag#v}"
}

unique() {
  local tag="$1"
  check "$tag" >/dev/null
  local existing
  existing="$(grep -E "$SEMVER" | sort -u || true)"
  if grep -qxF "$tag" <<<"$existing"; then
    die "$tag is already used by a tag or release (drafts count). Pick a new version, or delete the old draft."
  fi
  local highest
  highest="$(printf '%s\n%s\n' "$existing" "$tag" | grep -E "$SEMVER" | sed 's/^v//' | sort -t. -k1,1n -k2,2n -k3,3n | tail -n1)"
  if [[ "v$highest" != "$tag" ]]; then
    echo "warning: $tag is lower than v$highest, which already exists. Installed copies won't update to it." >&2
  fi
  echo "$tag is unique"
}

case "${1:-}" in
  check) check "${2:-}" ;;
  unique) unique "${2:-}" ;;
  *) echo "usage: $0 check|unique vX.Y.Z" >&2; exit 2 ;;
esac
