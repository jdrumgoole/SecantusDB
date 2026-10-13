"""A durable store trims its log while it is open.

WiredTiger removes a log file only after a checkpoint, and until 2026-10-13
the Python store checkpointed only at close, in a backup and in ``fsync``.
The Rust twin is ``crates/secantus-storage/tests/log_trim.rs``.
"""

from __future__ import annotations

import os
import time
from pathlib import Path

import pytest

from secantus.storage import Storage

PAD = "x" * 4096


def _log_files(home: Path) -> int:
    return sum(1 for name in os.listdir(home) if name.startswith("WiredTigerLog."))


def _fill_log(s: Storage, home: Path, at_least: int) -> int:
    """Write until the log has rolled over into several 10 MB files."""
    n = 0
    while _log_files(home) < at_least:
        inserted, errors = s.insert("db", "c", [{"_id": n + i, "pad": PAD} for i in range(50)])
        assert (inserted, errors) == (50, [])
        n += 50
        assert n < 200_000, "the log never rolled over"
    return n


def test_a_durable_store_removes_log_files_while_it_is_open(tmp_path: Path) -> None:
    s = Storage(str(tmp_path), durable=True, checkpoint_seconds=1)
    try:
        n = _fill_log(s, tmp_path, 4)
        deadline = time.monotonic() + 30
        while _log_files(tmp_path) >= 4 and time.monotonic() < deadline:
            time.sleep(0.2)
        assert _log_files(tmp_path) < 4
    finally:
        s.close()
    reopened = Storage(str(tmp_path), durable=True)
    try:
        assert reopened.count_matching("db", "c", {}) == n
    finally:
        reopened.close()


def test_without_a_checkpoint_the_log_files_stay(tmp_path: Path) -> None:
    # What every durable store did before: the interval set to "never".
    s = Storage(str(tmp_path), durable=True, checkpoint_seconds=0)
    try:
        _fill_log(s, tmp_path, 4)
        time.sleep(3)
        assert _log_files(tmp_path) >= 4
    finally:
        s.close()


@pytest.mark.skipif(
    os.environ.get("SECANTUS_FORCE_DURABLE") == "1",
    reason="SECANTUS_FORCE_DURABLE makes every store durable, so there is no fast store to test",
)
def test_a_fast_test_store_does_not_checkpoint_while_open(tmp_path: Path) -> None:
    s = Storage(str(tmp_path), durable=False, checkpoint_seconds=1)
    try:
        _fill_log(s, tmp_path, 4)
        time.sleep(3)
        assert _log_files(tmp_path) >= 4
    finally:
        s.close()
