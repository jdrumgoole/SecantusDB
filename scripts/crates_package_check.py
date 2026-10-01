"""Package every crates.io-bound MongoDB-side crate, as `cargo publish` would.

Phase B of ``tasks/rust-packages-plan.md``. ``cargo publish --dry-run`` on one
crate resolves its dependencies from crates.io, so it cannot check a crate
whose sibling crates are not published yet -- which, before the first release,
is all of them. ``cargo package --workspace`` packages several crates together
and resolves their dependencies on each other against the tarballs it just
made, then builds each one FROM ITS TARBALL. That is the property worth
checking: a crate that reaches for a file outside its own directory, a path
dependency without a version, or a missing ``include`` entry fails here rather
than on release day.

The crates live in several cargo workspaces today (the WiredTiger-linked ones
are excluded from ``crates/Cargo.toml``), so this stages copies of them into
one throwaway workspace first.

``PUBLISH_ORDER`` is the dependency order a release publishes in; keep new
crates.io-bound crates in it.

Usage: ``python scripts/crates_package_check.py [--no-verify] [--keep DIR]``
"""

from __future__ import annotations

import argparse
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
CRATES = REPO / "crates"

# Dependency order: every crate comes after everything it depends on.
PUBLISH_ORDER = (
    "secantus-wiredtiger-sys",
    "secantus-wt",
    "secantus-core",
    "secantus-auth",
    "secantus-wire",
    "secantus-commands",
    "secantus-server",
    "secantus-storage",
    "secantus-storage-adapter",
    "secantusdb",  # package name secantus-mdb
)

SKIP_DIRS = {"target", "target-dev"}


def _stage(dest: Path) -> None:
    for name in PUBLISH_ORDER:
        src = CRATES / name
        shutil.copytree(
            src,
            dest / name,
            ignore=lambda d, names: [n for n in names if n in SKIP_DIRS or n == "Cargo.lock"],
        )
        manifest = dest / name / "Cargo.toml"
        # Each WiredTiger-linked crate declares its own empty [workspace] so it
        # stays out of crates/Cargo.toml; inside the staging workspace that
        # would be a second root.
        text = re.sub(
            r"(?m)^\[workspace\]\n(?:[^\[\n][^\n]*\n|\n)*?(?=\[|\Z)", "", manifest.read_text()
        )
        manifest.write_text(text)
    members = ",\n".join(f'    "{n}"' for n in PUBLISH_ORDER)
    # Carry the tables the clean-workspace crates inherit (`dep.workspace =
    # true`) from crates/Cargo.toml; `cargo package` writes the resolved values
    # into each packaged manifest.
    root = (CRATES / "Cargo.toml").read_text()
    inherited = root[root.index("[workspace.dependencies]") :]
    (dest / "Cargo.toml").write_text(
        f'[workspace]\nresolver = "2"\nmembers = [\n{members}\n]\n\n{inherited}'
    )


def _forget_previous_runs() -> None:
    """Remove earlier runs' extracted tarballs of OUR crates from cargo's cache.

    `cargo package --workspace` unpacks each tarball into a local registry under
    `$CARGO_HOME/registry/{src,cache}/-<hash>/`, keyed by name and version. Our
    version does not change between commits, so a second run REUSES the first
    run's extraction and verifies the old content -- a gate that silently
    checks stale files (seen 2026-10-01: a fixed build.rs was verified as the
    broken one). Only the local-registry directories (named `-<hash>`) and only
    our crates' entries are removed.
    """
    home = Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo"))
    names = {
        (CRATES / n / "Cargo.toml").read_text().split('name = "', 1)[1].split('"', 1)[0]
        for n in PUBLISH_ORDER
    }
    for kind in ("src", "cache"):
        for registry in (home / "registry" / kind).glob("-*"):
            for entry in registry.iterdir():
                if any(entry.name.startswith(f"{n}-") for n in names):
                    if entry.is_dir():
                        shutil.rmtree(entry, ignore_errors=True)
                    else:
                        entry.unlink(missing_ok=True)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument(
        "--no-verify", action="store_true", help="package only; skip building each tarball"
    )
    ap.add_argument("--keep", type=Path, help="stage here and keep it, instead of a temp dir")
    args = ap.parse_args()

    if not (CRATES / "secantus-wiredtiger-sys" / "wiredtiger" / "CMakeLists.txt").exists():
        subprocess.run([sys.executable, str(REPO / "scripts" / "wt_sys_refresh.py")], check=True)

    stage = args.keep or Path(tempfile.mkdtemp(prefix="secantus-crates-"))
    if args.keep and stage.exists():
        shutil.rmtree(stage)
    stage.mkdir(parents=True, exist_ok=True)
    try:
        _forget_previous_runs()
        _stage(stage)
        cmd = ["cargo", "package", "--workspace", "--allow-dirty"]
        if args.no_verify:
            cmd.append("--no-verify")
        env = dict(os.environ)
        # The packaged crates must build from their own source: an inherited
        # prebuilt-WiredTiger override would hide a missing file in the tarball.
        for var in ("SECANTUS_WT_INCLUDE", "SECANTUS_WT_LIB"):
            env.pop(var, None)
        env.setdefault("CARGO_TARGET_DIR", str(stage / "target"))
        rc = subprocess.run(cmd, cwd=stage, env=env).returncode
        if rc == 0:
            crates = sorted((Path(env["CARGO_TARGET_DIR"]) / "package").glob("*.crate"))
            for c in crates:
                print(f"{c.stat().st_size / 1e6:6.2f} MB  {c.name}")
        return rc
    finally:
        if not args.keep:
            shutil.rmtree(stage, ignore_errors=True)


if __name__ == "__main__":
    raise SystemExit(main())
