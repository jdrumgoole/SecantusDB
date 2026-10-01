"""The WiredTiger copy in ``crates/secantus-wiredtiger-sys`` matches its stamp.

The crate builds WiredTiger from a pre-patched copy so a crates.io build needs
no Python. That copy must be what ``scripts/wt_sys_refresh.py`` produces from
the CURRENT ``vendor/wiredtiger`` and patch scripts, or the crate and the wheel
build different WiredTiger -- a storage-format risk, not a build nuisance. The
copy itself is gitignored; its digest is committed, and this regenerates the
copy into a temporary directory and compares.

A failure means: run ``./inv wt-sys-refresh`` and commit ``wiredtiger.sha256``.
"""

from __future__ import annotations

import importlib.util
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parent.parent


def _load_refresh():
    spec = importlib.util.spec_from_file_location(
        "wt_sys_refresh", REPO / "scripts" / "wt_sys_refresh.py"
    )
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def test_wt_sys_copy_matches_stamp(tmp_path: Path) -> None:
    refresh = _load_refresh()
    if not (refresh.VENDOR / "CMakeLists.txt").exists():
        pytest.fail("vendor/wiredtiger is not checked out; the drift check cannot run")
    got = refresh.refresh(tmp_path / "wiredtiger")
    want = refresh.STAMP.read_text().strip()
    assert got == want, (
        "crates/secantus-wiredtiger-sys is stale against vendor/wiredtiger or the "
        "cmake/patch_wt_*.py scripts: run `./inv wt-sys-refresh` and commit wiredtiger.sha256"
    )
