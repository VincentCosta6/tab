#!/bin/sh
# Writes public/THIRD_PARTY_LICENSES.txt: every license text (and NOTICE)
# of the Rust crates and npm packages tab ships. Vite copies public/ into
# the app; Settings → Updates links to it. release.yml runs this before
# building, which is how tab meets Apache-2.0's and MIT's attribution terms.
#
# Needs cargo-about (`cargo install --locked cargo-about --features cli`).
set -eu
cd "$(dirname "$0")/../.."
out=public/THIRD_PARTY_LICENSES.txt
mkdir -p public
cargo about generate --fail about.hbs -o "$out.rust"
node scripts/licenses/js-notices.mjs > "$out.js"
cat "$out.rust" "$out.js" > "$out"
rm -f "$out.rust" "$out.js"
echo "wrote $out ($(wc -c < "$out") bytes)"
