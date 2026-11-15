# tab's manifest marketplace

Manifests listed in `index.json` show up under Connections → Marketplace in
every tab built with the marketplace key. tab checks each one's SHA-256 and
Ed25519 signature before it installs it, then shows the person which tools
it will call. The manifest format is in `docs/recipe-manifests.md`.

Marketplace manifests can't run plugins, and may only call tools the server
marks read-only.

## Publishing a manifest

1. Add it to `manifests/<id>.json`, with a `version` (and an `author`).
   Test it first from your own folder: put it in `<app data>/recipes/` and
   press Reload on Connections.
2. Sign the folder with the marketplace key:

   ```sh
   TAB_MARKETPLACE_KEY=… cargo run -p tab-core --example manifest_sign -- index marketplace
   ```

   This rewrites `index.json`. Commit both, and open a pull request; review
   is the merge.

## The key

`cargo run -p tab-core --example manifest_sign -- keygen` makes one. The
private key (stdout) is kept outside the repo. Release builds get the
public key (stderr) as the build variable `TAB_MARKETPLACE_PUBKEY`. A build
without it has no marketplace. `TAB_MARKETPLACE_INDEX` points a build at
another index.
