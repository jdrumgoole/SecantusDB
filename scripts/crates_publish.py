"""Publish one release line's staged crates to crates.io, resumably.

Run by ``.github/workflows/publish-crates.yml`` after
``scripts/crates_package_check.py --keep <stage>`` has staged and verified the
crates. Publishes the line's crates (``--line mdb``: the MongoDB server's,
``--line pg``: the PostgreSQL server's) ONE AT A TIME in dependency order,
skipping any whose version is already on crates.io, so a run that failed part
way can simply be re-run on the same tag: crates.io versions are immutable, and
a plain ``cargo publish --workspace`` would stop at the first crate a previous
attempt had already uploaded. ``cargo publish`` waits for each crate to reach
the index before returning, so its dependents resolve.

The token comes from the environment (``CARGO_REGISTRY_TOKEN``, minted per run
by trusted publishing). Never run this by hand.

A PG release never publishes a MongoDB-side crate: the ones it depends on must
already be on crates.io at the pinned version (released by a ``secantusdb-v*``
tag first), and the run stops before publishing anything if one is missing.

Usage: ``python scripts/crates_publish.py <stage-dir> [--line {mdb,pg}] [--dry-run]``
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


def _check_module():
    spec = importlib.util.spec_from_file_location(
        "ccheck", REPO / "scripts" / "crates_package_check.py"
    )
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def _publish_order(line: str = "mdb") -> tuple[str, ...]:
    return _check_module().PUBLISHED[line]


def _required(line: str) -> tuple[str, ...]:
    """Staged crates this line depends on but never publishes itself."""
    mod = _check_module()
    return tuple(c for c in mod.STAGED[line] if c not in mod.PUBLISHED[line])


def _manifest(stage: Path, directory: str) -> tuple[str, str]:
    package = tomllib.loads((stage / directory / "Cargo.toml").read_text())["package"]
    return package["name"], package["version"]


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
    ap.add_argument("--line", choices=("mdb", "pg"), default="mdb")
    ap.add_argument("--dry-run", action="store_true")
    args = ap.parse_args()

    if args.dry_run:
        # A per-crate dry run cannot get past the first crate: the second
        # resolves the first from crates.io, where a dry run put nothing.
        # `--workspace` resolves the set against its own tarballs instead.
        cmd = ["cargo", "publish", "--workspace", "--allow-dirty", "--no-verify", "--dry-run"]
        subprocess.run(cmd, cwd=args.stage, check=True)
        return 0

    missing = [
        f"{name} {version}"
        for name, version in (_manifest(args.stage, d) for d in _required(args.line))
        if not published(name, version)
    ]
    if missing:
        print(
            f"the {args.line} line depends on crates not on crates.io yet:",
            *missing,
            "release them first (their own tag); nothing was published",
            sep="\n  ",
        )
        return 1

    for directory in _publish_order(args.line):
        name, version = _manifest(args.stage, directory)
        if published(name, version):
            print(f"already on crates.io: {name} {version}")
            continue
        cmd = ["cargo", "publish", "-p", name, "--allow-dirty", "--no-verify"]
        print(f"publishing {name} {version}", flush=True)
        subprocess.run(cmd, cwd=args.stage, check=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
