"""No build output is tracked in git.

The sdist is built from the tracked tree. On 2026-10-07 the ``0.7.0b0`` sdist
came out at 573 MB (``0.6.0b17``'s was 14 MB) and PyPI refused it: a cargo
target dir, ``crates/secantus-storage/target-dev/``, had been committed a week
earlier -- 6,875 files, 1.77 GB -- and nothing noticed until the upload.
"""

from __future__ import annotations

import pathlib
import re
import shutil
import subprocess

import pytest

_REPO = pathlib.Path(__file__).resolve().parent.parent

#: A cargo target dir under ``crates/`` -- ``target``, ``target-dev``, ...
_BUILD_DIR = re.compile(r"^crates/(?:[^/]+/)*target[^/]*/")

#: Far above any source file here (the largest is ~1 MB) and far below the
#: 20 MB+ rlibs and test binaries a target dir holds.
_MAX_TRACKED_BYTES = 8 * 1024 * 1024


def _tracked() -> list[tuple[int, str]]:
    if shutil.which("git") is None or not (_REPO / ".git").exists():
        pytest.skip("not a git checkout")
    out = subprocess.run(
        ["git", "ls-tree", "-r", "-l", "-z", "HEAD"],
        cwd=_REPO,
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    rows = []
    for entry in out.split("\0"):
        if not entry:
            continue
        meta, path = entry.split("\t", 1)
        size = meta.split()[3]
        rows.append((0 if size == "-" else int(size), path))  # "-" is a submodule
    return rows


def test_no_cargo_target_dir_is_tracked() -> None:
    tracked = [path for _, path in _tracked() if _BUILD_DIR.match(path)]
    assert tracked == [], f"{len(tracked)} build-output file(s) tracked, e.g. {tracked[:3]}"


def test_no_oversized_file_is_tracked() -> None:
    big = [(size, path) for size, path in _tracked() if size > _MAX_TRACKED_BYTES]
    assert big == [], f"tracked files over {_MAX_TRACKED_BYTES} bytes: {sorted(big)[-3:]}"
