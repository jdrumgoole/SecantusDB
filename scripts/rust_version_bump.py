"""Bump one Rust version line (MongoDB or PostgreSQL), pins and lockfiles included.

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

The PostgreSQL crates (``secantus-pgcatalog``, ``secantus-pgplan``,
``secantus-pgwire``, ``secantus-pg``) carry their own lockstep line, bumped
with ``--line pg``; each line's bump leaves the other alone, because the two
version strings never coincide. A PG crate's ``=`` pins on MongoDB-side crates
(``secantus-core``, ``-storage``, ``-auth``) belong to the MongoDB line and move
with it.

The bump also rewrites the version a READER is told to install. Every release
so far is a pre-release, so the README, the docs and the crate READMEs print
``cargo install secantus-mdb --version <ver>`` and ``secantus-mdb = "<ver>"``
with the version spelled out, and nothing else moves those when the crates do.
Only a version attached to the line's own installable crate is rewritten; a
version mentioned in passing is history and stays.

Usage::

    python scripts/rust_version_bump.py 0.5.3-beta.166             # MongoDB line
    python scripts/rust_version_bump.py --line pg 0.1.0-beta.3     # PostgreSQL line
    python scripts/rust_version_bump.py --check                    # check only
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
CRATES = REPO / "crates"
# The manifest whose version IS each line's version.
LINES = {"mdb": "secantusdb", "pg": "secantus-pg"}
CANONICAL = CRATES / LINES["mdb"] / "Cargo.toml"

# The crate a user installs for each line, as the docs name it.
INSTALL_CRATE = {"mdb": "secantus-mdb", "pg": "secantus-pg"}
# The pages that print an install command. The changelog and the blog are
# history; the site reads its version from the binary tag in pelicanconf.py,
# which moves only once the binaries are published.
DOC_GLOBS = ("README.md", "docs/*.md", "docs-rust/*.md", "crates/*/README.md")
DOC_HISTORY = {"docs/changelog.md"}

# A SemVer pre-release of the shape the lockstep line uses.
VERSION_RE = re.compile(r"^\d+\.\d+\.\d+(-[0-9A-Za-z.]+)?$")


def current_version(crates: Path = CRATES, line: str = "mdb") -> str:
    directory = LINES[line]
    text = (crates / directory / "Cargo.toml").read_text()
    m = re.search(r'(?m)^version = "([^"]+)"', text)
    if not m:
        raise SystemExit(f"no version in crates/{directory}/Cargo.toml")
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


def doc_files(repo: Path = REPO) -> list[Path]:
    """Every published page that may print an install command."""
    files = {p for pattern in DOC_GLOBS for p in repo.glob(pattern)}
    return sorted(p for p in files if p.relative_to(repo).as_posix() not in DOC_HISTORY)


def _install_pin(crate: str, version: str | None = None) -> re.Pattern[str]:
    """`<crate> --version <ver>` or `<crate> = "<ver>"`; the version is group 2.

    The gap may be a line break (prose wraps) and, at the start of the next
    line, the `>` of a Markdown quote. A longer crate name that merely starts
    the same (`secantus-pgwire`) and a longer version do not match.
    """
    ver = re.escape(version) if version else r"\d+\.\d+\.\d+(?:-[0-9A-Za-z.]*[0-9A-Za-z])?"
    return re.compile(
        r"((?<![\w-])" + re.escape(crate) + r'(?:\s+(?:>\s*)?--version\s+|\s*=\s*"))'
        r"(" + ver + r")(?![0-9A-Za-z-]|\.[0-9A-Za-z])"
    )


def bump_docs(old: str, new: str, line: str = "mdb", repo: Path = REPO) -> list[Path]:
    """Rewrite the install version the docs print for `line`; return files changed."""
    pattern = _install_pin(INSTALL_CRATE[line], old)
    changed = []
    for path in doc_files(repo):
        text = path.read_text(encoding="utf-8")
        updated = pattern.sub(lambda m: m.group(1) + new, text)
        if updated != text:
            path.write_text(updated, encoding="utf-8")
            changed.append(path)
    return changed


def doc_mismatches(line: str = "mdb", repo: Path = REPO, crates: Path | None = None) -> list[str]:
    """Install commands in the docs naming a version other than the line's own."""
    want = current_version(crates or repo / "crates", line)
    pattern = _install_pin(INSTALL_CRATE[line])
    found = []
    for path in doc_files(repo):
        text = path.read_text(encoding="utf-8")
        for m in pattern.finditer(text):
            if m.group(2) != want:
                n = text.count("\n", 0, m.start(2)) + 1
                found.append(f"{path.relative_to(repo)}:{n}: names {m.group(2)}, crate is {want}")
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
        "--line",
        choices=sorted(LINES),
        default="mdb",
        help="which version line to bump: mdb (default) or pg",
    )
    ap.add_argument(
        "--check", action="store_true", help="only check the lockfiles resolve --locked"
    )
    args = ap.parse_args()

    old = current_version(line=args.line)
    if args.line == "pg" and old == current_version(line="mdb"):
        ap.error("the two version lines share a version; a bump would move both")
    if args.check:
        bad = lock_failures()
    else:
        if not args.new or not VERSION_RE.match(args.new):
            ap.error("give the new version, e.g. 0.5.3-beta.166")
        if args.new == old:
            ap.error(f"already at {old}")
        changed = bump(old, args.new)
        docs = bump_docs(old, args.new, line=args.line)
        print(f"{old} -> {args.new}: {len(changed)} files, {len(docs)} doc pages")
        stale = leftovers(old) + doc_mismatches(line=args.line)
        if stale:
            print(
                "a version other than the new one survived in:", *stale, sep="\n  ", file=sys.stderr
            )
            return 1
        bad = lock_failures()
    if bad:
        print("lockfiles that do not resolve --locked:", *bad, sep="\n  ", file=sys.stderr)
        return 1
    print("every lockfile resolves --locked")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
