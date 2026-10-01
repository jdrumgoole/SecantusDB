"""Publish the staged MongoDB-side crates to crates.io, resumably.

Run by ``.github/workflows/publish-crates.yml`` after
``scripts/crates_package_check.py --keep <stage>`` has staged and verified the
crates. Publishes them ONE AT A TIME in ``PUBLISH_ORDER`` (dependency order),
skipping any whose version is already on crates.io, so a run that failed part
way can simply be re-run on the same tag: crates.io versions are immutable, and
a plain ``cargo publish --workspace`` would stop at the first crate a previous
attempt had already uploaded. ``cargo publish`` waits for each crate to reach
the index before returning, so its dependents resolve.

The token comes from the environment (``CARGO_REGISTRY_TOKEN``, minted per run
by trusted publishing). Never run this by hand.

Usage: ``python scripts/crates_publish.py <stage-dir> [--dry-run]``
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import subprocess
import urllib.error
import urllib.request
from pathlib import Path

import tomllib

REPO = Path(__file__).resolve().parent.parent
UA = "secantusdb-publish (https://github.com/jdrumgoole/SecantusDB)"


def _publish_order() -> tuple[str, ...]:
    spec = importlib.util.spec_from_file_location(
        "ccheck", REPO / "scripts" / "crates_package_check.py"
    )
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod.PUBLISH_ORDER


def published(name: str, version: str) -> bool:
    req = urllib.request.Request(
        f"https://crates.io/api/v1/crates/{name}/{version}", headers={"User-Agent": UA}
    )
    try:
        with urllib.request.urlopen(req) as r:
            return json.load(r).get("version", {}).get("num") == version
    except urllib.error.HTTPError as e:
        if e.code == 404:
            return False
        raise


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("stage", type=Path)
    ap.add_argument("--dry-run", action="store_true")
    args = ap.parse_args()

    if args.dry_run:
        # A per-crate dry run cannot get past the first crate: the second
        # resolves the first from crates.io, where a dry run put nothing.
        # `--workspace` resolves the set against its own tarballs instead.
        cmd = ["cargo", "publish", "--workspace", "--allow-dirty", "--no-verify", "--dry-run"]
        subprocess.run(cmd, cwd=args.stage, check=True)
        return 0

    for directory in _publish_order():
        manifest = tomllib.loads((args.stage / directory / "Cargo.toml").read_text())["package"]
        name, version = manifest["name"], manifest["version"]
        if published(name, version):
            print(f"already on crates.io: {name} {version}")
            continue
        cmd = ["cargo", "publish", "-p", name, "--allow-dirty", "--no-verify"]
        print(f"publishing {name} {version}", flush=True)
        subprocess.run(cmd, cwd=args.stage, check=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
