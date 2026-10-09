"""`cmake/patch_wt_fsync_group.py` -- the WiredTiger patch that credits one
`fsync` with every commit already written (`method=fsync`).

It rewrites the sync of the storage engine's log, so the script is held to
more than "it ran": it must apply to the vendored source as it is today, on
its own and after the dsync patch (the order both builds use), be a no-op the
second time, and refuse to write a file it could only half patch.

Whether the patched engine keeps its durability promise is NOT something a
test here can show: a killed process leaves its written bytes in the page
cache, so only a machine that loses power tells a synced commit from a
written one. That was measured by hard-rebooting a server under sixteen
remote writers (`tasks/backlog.md`, the Linux entry).
"""

from __future__ import annotations

import shutil
import subprocess
import sys
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parent.parent
SCRIPT = REPO / "cmake" / "patch_wt_fsync_group.py"
DSYNC_SCRIPT = REPO / "cmake" / "patch_wt_dsync_group.py"
LOG_C = REPO / "vendor" / "wiredtiger" / "src" / "log" / "log.c"
MARKER = "/* secantus-patch: fsync covers what is written */"


def _run(script: Path, target: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(script), str(target)], capture_output=True, text=True, check=False
    )


@pytest.fixture
def log_c(tmp_path: Path) -> Path:
    if not LOG_C.exists():
        pytest.fail("vendor/wiredtiger is not checked out; the patch cannot be checked")
    target = tmp_path / "log.c"
    shutil.copy(LOG_C, target)
    return target


@pytest.mark.parametrize("after_dsync", [False, True])
def test_patch_applies_to_the_vendored_source_and_is_idempotent(
    log_c: Path, after_dsync: bool
) -> None:
    if after_dsync:
        assert _run(DSYNC_SCRIPT, log_c).returncode == 0
    before = log_c.read_text()
    assert MARKER not in before

    first = _run(SCRIPT, log_c)
    assert first.returncode == 0, first.stderr
    patched = log_c.read_text()
    assert patched.count(MARKER) == 3
    # Stock lines survive in order, except the three the patch rewrites: the
    # 10 ms wait, and the unconditional sync it wraps in a block.
    rewritten = {
        "            __wt_cond_wait(session, log->log_sync_cond, 10 * WT_THOUSAND, NULL);",
        "        if (F_ISSET_ATOMIC_16(slot, WT_SLOT_SYNC))",
        '            WT_ERR(__log_fsync_file(session, &sync_lsn, "log_release", false));',
    }
    it = iter(patched.splitlines())
    kept = [line for line in before.splitlines() if line not in rewritten]
    assert all(line in it for line in kept)

    second = _run(SCRIPT, log_c)
    assert second.returncode == 0, second.stderr
    assert "already patched" in second.stdout
    assert log_c.read_text() == patched


def test_the_sync_is_credited_only_with_what_was_written_before_it(log_c: Path) -> None:
    """The durability argument, as text: the LSN is read BEFORE the sync, and
    credited only when this thread made the sync, in the same log file, with
    that file still the current one."""
    assert _run(SCRIPT, log_c).returncode == 0
    text = log_c.read_text()
    noted = text.index("WT_ASSIGN_LSN(&written_lsn, &log->write_lsn);")
    synced = text.index('WT_ERR(__log_fsync_file(session, &sync_lsn, "log_release", false));')
    credited = text.index("WT_ASSIGN_LSN(&log->sync_lsn, &written_lsn);")
    assert noted < synced < credited
    guard = text[synced:credited]
    for condition in (
        "own_sync",
        "written_lsn.l.file == sync_lsn.l.file",
        "log->fileid == sync_lsn.l.file",
        "__wt_log_cmp(&log->sync_lsn, &sync_lsn) == 0",
    ):
        assert condition in guard, condition


def test_a_missing_anchor_leaves_the_file_untouched(log_c: Path) -> None:
    """A WiredTiger bump that moves one anchor must fail the build, not ship a
    sync path with two of three edits applied."""
    broken = log_c.read_text().replace(
        '            WT_ERR(__log_fsync_file(session, &sync_lsn, "log_release", false));\n',
        '            WT_ERR(__log_fsync_file(session, &sync_lsn, "release", false));\n',
    )
    log_c.write_text(broken)
    result = _run(SCRIPT, log_c)
    assert result.returncode == 1
    assert "anchor found 0 times" in result.stderr
    assert log_c.read_text() == broken


def test_an_unknown_file_is_refused(tmp_path: Path) -> None:
    other = tmp_path / "log_slot.c"
    other.write_text("int x;\n")
    result = _run(SCRIPT, other)
    assert result.returncode == 1
    assert other.read_text() == "int x;\n"
