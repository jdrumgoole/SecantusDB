"""`cmake/patch_wt_dsync_group.py` -- the one WiredTiger patch that changes
BEHAVIOUR (group commit for `method=dsync`) rather than the build.

It rewrites the commit path of the storage engine, so the script itself is
held to more than "it ran": it must apply to the vendored source as it is
today, be a no-op the second time, and refuse to write a file it could only
half patch. Whether the patched engine keeps its durability promise is the job
of the SIGKILL tests in `tests/test_rust_pgserver_slice.py`.
"""

from __future__ import annotations

import shutil
import subprocess
import sys
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parent.parent
SCRIPT = REPO / "cmake" / "patch_wt_dsync_group.py"
LOG_DIR = REPO / "vendor" / "wiredtiger" / "src" / "log"
MARKER = "/* secantus-patch: dsync group commit */"
#: Markers each file carries once patched: one per edit.
EXPECTED_MARKERS = {"log.c": 5, "log_slot.c": 1}


def _run(target: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(SCRIPT), str(target)], capture_output=True, text=True, check=False
    )


@pytest.fixture
def sources(tmp_path: Path) -> dict[str, Path]:
    if not (LOG_DIR / "log.c").exists():
        pytest.fail("vendor/wiredtiger is not checked out; the patch cannot be checked")
    out = {}
    for name in EXPECTED_MARKERS:
        out[name] = tmp_path / name
        shutil.copy(LOG_DIR / name, out[name])
    return out


@pytest.mark.parametrize("name", sorted(EXPECTED_MARKERS))
def test_patch_applies_to_the_vendored_source_and_is_idempotent(
    sources: dict[str, Path], name: str
) -> None:
    target = sources[name]
    pristine = target.read_text()
    assert MARKER not in pristine, "the vendored source must be unpatched"

    first = _run(target)
    assert first.returncode == 0, first.stderr
    patched = target.read_text()
    assert patched.count(MARKER) == EXPECTED_MARKERS[name]
    # Nothing is removed: every original line survives, in order.
    it = iter(patched.splitlines())
    assert all(line in it for line in pristine.splitlines())

    second = _run(target)
    assert second.returncode == 0, second.stderr
    assert "already patched" in second.stdout
    assert target.read_text() == patched


def test_the_patch_only_groups_dsync_commits(sources: dict[str, Path]) -> None:
    """`method=fsync` (both MongoDB servers) and unsynced commits must keep the
    stock path: the grouping is gated on `WT_LOG_DSYNC`, and the directory-sync
    shortcut on the slot NOT needing a file sync."""
    target = sources["log.c"]
    assert _run(target).returncode == 0
    text = target.read_text()
    assert (
        "grouped = LF_ISSET(WT_LOG_DSYNC) && LF_ISSET(WT_LOG_FLUSH) && !LF_ISSET(WT_LOG_FSYNC)"
        in text
    )
    assert "if (!F_ISSET_ATOMIC_16(slot, WT_SLOT_SYNC) &&" in text


def test_a_missing_anchor_leaves_the_file_untouched(sources: dict[str, Path]) -> None:
    """A WiredTiger bump that moves one anchor must fail the build, not ship a
    commit path with four of five edits applied."""
    target = sources["log.c"]
    broken = target.read_text().replace(
        "    force = LF_ISSET(WT_LOG_FLUSH | WT_LOG_FSYNC);\n", "    force = 0;\n"
    )
    target.write_text(broken)
    result = _run(target)
    assert result.returncode == 1
    assert "anchor found 0 times" in result.stderr
    assert target.read_text() == broken


def test_an_unknown_file_is_refused(tmp_path: Path) -> None:
    other = tmp_path / "log_sys.c"
    other.write_text("int x;\n")
    result = _run(other)
    assert result.returncode == 1
    assert other.read_text() == "int x;\n"
