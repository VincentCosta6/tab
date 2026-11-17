#!/usr/bin/env python3
"""Checks that every Zed crate tab pins is still Apache-2.0 on Zed's main.

tab may only depend on Zed's Apache-2.0 crates (see CLAUDE.md, "Third-party
code"). The list comes from Cargo.lock at run time: every package whose
source is https://github.com/zed-industries/zed. For each, this reads
crates/<name>/Cargo.toml on Zed's main and looks at its `license` field.

Prints what it found. If any crate is no longer plainly Apache-2.0 (or its
manifest can't be found), writes a Markdown report to the path given as the
first argument, for the workflow to put in an issue. Exits 0 either way;
exits 1 only if it couldn't run at all.

Usage: python3 scripts/zed-license-watch.py [report.md]
"""

import sys
import tomllib
import urllib.error
import urllib.request

ZED = "https://github.com/zed-industries/zed"
RAW = "https://raw.githubusercontent.com/zed-industries/zed/main"
ALLOWED = {"Apache-2.0"}


def pinned(lock_path="Cargo.lock"):
    """(name, rev) for each Zed crate in Cargo.lock."""
    with open(lock_path, "rb") as f:
        lock = tomllib.load(f)
    found = set()
    for pkg in lock.get("package", []):
        source = pkg.get("source", "")
        if source.startswith(f"git+{ZED}"):
            rev = source.rsplit("#", 1)[-1]
            found.add((pkg["name"], rev))
    return sorted(found)


def license_on_main(name):
    """The crate's `license` on Zed's main, or None if its manifest isn't
    where Zed keeps crates (crates/<name>/Cargo.toml)."""
    for folder in dict.fromkeys([name, name.replace("-", "_"), name.replace("_", "-")]):
        url = f"{RAW}/crates/{folder}/Cargo.toml"
        try:
            with urllib.request.urlopen(url, timeout=30) as r:
                manifest = tomllib.loads(r.read().decode())
        except urllib.error.HTTPError as e:
            if e.code == 404:
                continue
            raise
        if manifest.get("package", {}).get("name") != name:
            continue
        lic = manifest["package"].get("license")
        # `license.workspace = true` (or a missing field) isn't a plain
        # Apache-2.0 line; report it so someone looks.
        return lic if isinstance(lic, str) else f"not set in the crate ({lic!r})"
    return None


def main():
    crates = pinned()
    if not crates:
        print("tab pins no Zed crates; nothing to check.")
        return 0
    problems = []
    for name, rev in crates:
        lic = license_on_main(name)
        print(f"{name} (pinned at {rev}): {lic or 'manifest not found on main'}")
        if lic is None:
            problems.append(f"- `{name}` (pinned at `{rev}`): no `crates/{name}/Cargo.toml` on Zed's main, so its license can't be checked. It may have moved or been removed.")
        elif lic.strip() not in ALLOWED:
            problems.append(f"- `{name}` (pinned at `{rev}`): license on Zed's main is now `{lic}`.")
    if problems and len(sys.argv) > 1:
        with open(sys.argv[1], "w") as f:
            f.write(
                "The weekly Zed license check found Zed crates tab pins that are no longer plainly "
                "Apache-2.0 on Zed's `main`:\n\n"
                + "\n".join(problems)
                + "\n\ntab may only use Zed's Apache-2.0 crates (CLAUDE.md, \"Third-party code\"). "
                "The pinned revs keep their old license, but don't bump past the change: "
                "check the crate's history, and plan to replace it or stay on the last Apache-2.0 rev.\n"
            )
    print(f"{len(problems)} problem(s).")
    return 0


if __name__ == "__main__":
    sys.exit(main())
