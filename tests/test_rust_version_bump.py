"""`scripts/rust_version_bump.py` rewrites the whole MongoDB-side version line.

Run against a COPY of the real crates/ manifests and lockfiles, so the test
sees every place the version actually appears today -- the `=` pins added for
crates.io included -- rather than a hand-written fixture that would agree with
the script by construction.
"""

from __future__ import annotations

import importlib.util
import shutil
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent


def _load():
    spec = importlib.util.spec_from_file_location(
        "rust_version_bump", REPO / "scripts" / "rust_version_bump.py"
    )
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def _copy_manifests(dest: Path) -> Path:
    crates = dest / "crates"
    src = REPO / "crates"
    for path in [*src.glob("Cargo.*"), *src.glob("*/Cargo.toml"), *src.glob("*/Cargo.lock")]:
        target = crates / path.relative_to(src)
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(path, target)
    return crates


def test_bump_rewrites_versions_pins_and_locks(tmp_path: Path) -> None:
    mod = _load()
    crates = _copy_manifests(tmp_path)
    old = mod.current_version(crates)
    new = "9.9.9-beta.1"  # shares no prefix with any real version

    changed = mod.bump(old, new, crates)

    assert mod.current_version(crates) == new
    assert mod.leftovers(old, crates) == []
    names = {p.relative_to(crates).as_posix() for p in changed}
    # A pin, a lockfile of a crate OUTSIDE the lockstep that depends on it,
    # and the canonical manifest all move together.
    assert "secantus-wt/Cargo.toml" in names  # pins secantus-wiredtiger-sys
    assert "secantus-pg/Cargo.lock" in names  # records secantus-storage
    assert "secantusdb/Cargo.toml" in names
    pins = (crates / "secantus-commands" / "Cargo.toml").read_text()
    assert f'version = "={new}"' in pins and f"={old}" not in pins


def test_bump_leaves_the_pg_version_line_alone(tmp_path: Path) -> None:
    mod = _load()
    crates = _copy_manifests(tmp_path)
    pg_old = mod.current_version(crates, line="pg")
    old = mod.current_version(crates)
    mod.bump(old, old + ".9", crates)
    # The PG crates' own version and their pins on each other stay; only
    # their pins on MongoDB-side crates (secantus-core, -storage, -auth) move.
    assert mod.current_version(crates, line="pg") == pg_old
    pg = (crates / "secantus-pg" / "Cargo.toml").read_text()
    assert f'"={pg_old}"' in pg and f'"={old}.9"' in pg


def test_pg_line_bumps_the_four_pg_crates_and_nothing_else(tmp_path: Path) -> None:
    mod = _load()
    crates = _copy_manifests(tmp_path)
    mdb_before = (crates / "secantusdb" / "Cargo.toml").read_text()
    mdb = mod.current_version(crates)
    old = mod.current_version(crates, line="pg")
    assert old != mdb
    new = "9.8.7-beta.1"

    changed = mod.bump(old, new, crates)

    assert mod.current_version(crates, line="pg") == new
    assert mod.current_version(crates) == mdb
    assert mod.leftovers(old, crates) == []
    names = {p.relative_to(crates).as_posix() for p in changed}
    for manifest in ("secantus-pgcatalog", "secantus-pgplan", "secantus-pgwire", "secantus-pg"):
        assert f"{manifest}/Cargo.toml" in names
    assert "secantus-pg/Cargo.lock" in names
    assert (crates / "secantusdb" / "Cargo.toml").read_text() == mdb_before
    pg = (crates / "secantus-pg" / "Cargo.toml").read_text()
    # Its pins on the PG line moved; its pins on the MongoDB line did not.
    assert f'version = "={new}"' in pg and f'"={mdb}"' in pg


def test_bump_matches_whole_versions_only(tmp_path: Path) -> None:
    mod = _load()
    crates = tmp_path / "crates"
    (crates / "secantusdb").mkdir(parents=True)
    (crates / "secantusdb" / "Cargo.toml").write_text(
        '[package]\nversion = "1.0.0-beta.16"\n'
        '[dependencies]\nx = { version = "=1.0.0-beta.165" }\n'
    )
    mod.bump("1.0.0-beta.16", "1.0.0-beta.17", crates)
    text = (crates / "secantusdb" / "Cargo.toml").read_text()
    assert 'version = "1.0.0-beta.17"' in text
    assert "=1.0.0-beta.165" in text  # a longer version that merely starts the same is untouched
