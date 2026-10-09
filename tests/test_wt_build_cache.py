"""`./inv rust-wt-build` recovers from a build directory left by another source tree.

CMake will not reconfigure a build directory whose cache names a different
source ("does not match the source ... used to generate cache"). The Rust-side
WiredTiger build moved its source from ``vendor/wiredtiger`` to a patched copy
on 2026-10-07, so every checkout built before that failed there until someone
deleted ``build/rust-wt/wt-build`` by hand.
"""

from __future__ import annotations

from pathlib import Path

import rust_tasks


def _configured_from(build: Path, source: Path) -> None:
    build.mkdir(parents=True)
    (build / "CMakeCache.txt").write_text(
        f"# This is the CMakeCache file.\nCMAKE_HOME_DIRECTORY:INTERNAL={source}\n"
    )
    (build / "libwiredtiger.a").write_bytes(b"old")


def test_a_cache_from_another_source_tree_is_removed(tmp_path: Path) -> None:
    build, src = tmp_path / "wt-build", tmp_path / "wt-src"
    src.mkdir()
    _configured_from(build, tmp_path / "vendor" / "wiredtiger")

    assert rust_tasks._drop_foreign_cmake_cache(build, src) is True
    assert not build.exists()


def test_a_cache_from_the_same_source_tree_is_kept(tmp_path: Path) -> None:
    build, src = tmp_path / "wt-build", tmp_path / "wt-src"
    src.mkdir()
    _configured_from(build, src)

    assert rust_tasks._drop_foreign_cmake_cache(build, src) is False
    assert (build / "libwiredtiger.a").read_bytes() == b"old"


def test_a_directory_cmake_never_configured_is_left_alone(tmp_path: Path) -> None:
    build, src = tmp_path / "wt-build", tmp_path / "wt-src"
    src.mkdir()
    build.mkdir()
    (build / "notes.txt").write_text("not a cmake build")

    assert rust_tasks._drop_foreign_cmake_cache(build, src) is False
    assert (build / "notes.txt").exists()
