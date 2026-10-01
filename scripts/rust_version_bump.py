"""Bump the Rust MongoDB server's lockstep version, pins and lockfiles included.

Phase E step 2 of ``tasks/rust-packages-plan.md``. The MongoDB-side crates all
carry one version (``0.MAJOR.PATCH-beta.N``) and pin each other EXACTLY
(``secantus-core = { version = "=0.5.3-beta.165", ... }``), because crates.io
needs a version on every dependency of a published crate. So a release bump has
to rewrite three things in step: each ``[package] version``, each ``=`` pin,
and every ``Cargo.lock`` that records a MongoDB-side crate -- including the PG
server's and the Python bindings', which depend on ``secantus-storage``.

The rewrite is a substitution of the exact old version string, which is what
the documented ``sed`` one-liner did. What this adds is the CHECK: after the
rewrite, the old version must appear nowhere it was replaced, and every
lockfile must still resolve ``--locked`` -- the release workflows build
``--locked``, so a lockfile the bump left inconsistent fails on release day.

The PostgreSQL crates carry their own version line and are left alone: their
``[package] version`` never equals the MongoDB one.

Usage::

    python scripts/rust_version_bump.py 0.5.3-beta.166        # bump + check
    python scripts/rust_version_bump.py --check               # check only
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
CRATES = REPO / "crates"
CANONICAL = CRATES / "secantusdb" / "Cargo.toml"

# A SemVer pre-release of the shape the lockstep line uses.
VERSION_RE = re.compile(r"^\d+\.\d+\.\d+(-[0-9A-Za-z.]+)?$")


def current_version(crates: Path = CRATES) -> str:
    text = (crates / "secantusdb" / "Cargo.toml").read_text()
    m = re.search(r'(?m)^version = "([^"]+)"', text)
    if not m:
        raise SystemExit("no version in crates/secantusdb/Cargo.toml")
    return m.group(1)


def touched_files(crates: Path = CRATES) -> list[Path]:
    """Every crate manifest and lockfile one level under ``crates/``."""
    out = []
    for d in sorted(p for p in crates.iterdir() if p.is_dir()):
        for name in ("Cargo.toml", "Cargo.lock"):
            if (d / name).exists():
                out.append(d / name)
    for name in ("Cargo.toml", "Cargo.lock"):
        if (crates / name).exists():
            out.append(crates / name)
    return out


def bump(old: str, new: str, crates: Path = CRATES) -> list[Path]:
    """Replace `old` with `new` in every manifest and lockfile; return those changed.

    Matches the version as a whole token -- quoted, or after `=` in a pin -- so
    `0.5.3-beta.16` never rewrites inside `0.5.3-beta.165`.
    """
    pattern = re.compile(r'(?<=["=])' + re.escape(old) + r'(?=")')
    changed = []
    for path in touched_files(crates):
        text = path.read_text()
        updated = pattern.sub(new, text)
        if updated != text:
            path.write_text(updated)
            changed.append(path)
    return changed


def leftovers(old: str, crates: Path = CRATES) -> list[str]:
    """Places the old version still appears as a whole token."""
    pattern = re.compile(r'(?<=["=])' + re.escape(old) + r'(?=")')
    found = []
    for path in touched_files(crates):
        for n, line in enumerate(path.read_text().splitlines(), 1):
            if pattern.search(line):
                found.append(f"{path.relative_to(crates.parent)}:{n}: {line.strip()}")
    return found


def lock_failures(crates: Path = CRATES) -> list[str]:
    """Crate dirs whose Cargo.lock no longer resolves `--locked`."""
    bad = []
    for lock in [p for p in touched_files(crates) if p.name == "Cargo.lock"]:
        proc = subprocess.run(
            ["cargo", "metadata", "--locked", "--format-version", "1", "--offline"],
            cwd=lock.parent,
            capture_output=True,
            text=True,
        )
        if proc.returncode != 0:
            # --offline can fail only for want of a download, which is not a
            # lockfile problem; retry online before calling it one.
            proc = subprocess.run(
                ["cargo", "metadata", "--locked", "--format-version", "1"],
                cwd=lock.parent,
                capture_output=True,
                text=True,
            )
        if proc.returncode != 0:
            last = (proc.stderr.strip().splitlines() or ["?"])[-1]
            bad.append(f"{lock.relative_to(crates.parent)}: {last}")
    return bad


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("new", nargs="?", help="the new lockstep version, e.g. 0.5.3-beta.166")
    ap.add_argument(
        "--check", action="store_true", help="only check the lockfiles resolve --locked"
    )
    args = ap.parse_args()

    old = current_version()
    if args.check:
        bad = lock_failures()
    else:
        if not args.new or not VERSION_RE.match(args.new):
            ap.error("give the new version, e.g. 0.5.3-beta.166")
        if args.new == old:
            ap.error(f"already at {old}")
        changed = bump(old, args.new)
        print(f"{old} -> {args.new}: {len(changed)} files")
        stale = leftovers(old)
        if stale:
            print("the old version survived in:", *stale, sep="\n  ", file=sys.stderr)
            return 1
        bad = lock_failures()
    if bad:
        print("lockfiles that do not resolve --locked:", *bad, sep="\n  ", file=sys.stderr)
        return 1
    print("every lockfile resolves --locked")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
