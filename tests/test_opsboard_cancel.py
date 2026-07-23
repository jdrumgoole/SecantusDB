"""Cancel / process-tree teardown tests — real spawned processes, no mock."""

from __future__ import annotations

import os
import subprocess
import sys
import time
from pathlib import Path

import pytest

from secantus.jobkit import CANCELLED, Journal
from secantus.opsboard.runner import JobRunner

pytestmark = pytest.mark.skipif(
    sys.platform == "win32",
    reason="cancel uses POSIX process groups (killpg); the board targets macOS/Linux",
)


def _group_dead(pgid: int, timeout: float = 5.0) -> bool:
    """True once the process GROUP is empty (killpg(pgid, 0) → ProcessLookupError).

    A killed process we spawned lingers as a zombie (still in its group) until
    reaped, so callers must ``proc.wait()`` the leader first; grandchildren are
    reparented to init and reaped by it.
    """
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            os.killpg(pgid, 0)
        except ProcessLookupError:
            return True
        time.sleep(0.05)
    return False


def _spawn_tree() -> subprocess.Popen:
    # A shell that forks a child sleep — a 2-process group to tear down.
    return subprocess.Popen(
        ["sh", "-c", "sleep 60 & sleep 60"],
        start_new_session=True,  # own process group (as jobkit does)
    )


def _running_job(journal: Journal, pid: int) -> int:
    return journal.create(target="python", task="test", argv=["test"], worktree="/w", host_pid=pid)


def test_cancel_kills_the_process_group(tmp_path: Path) -> None:
    proc = _spawn_tree()
    try:
        journal = Journal(tmp_path / "opsboard.db")
        runner = JobRunner(repo_root=tmp_path, journal=journal)
        jid = _running_job(journal, proc.pid)

        assert runner.cancel(jid, grace=0.3) is True
        # Reap the killed leader (our child), then the whole group is empty.
        assert proc.wait(timeout=5) is not None
        assert proc.returncode != 0  # killed by signal
        assert _group_dead(proc.pid), "process group not fully torn down"
    finally:
        if proc.poll() is None:
            proc.kill()
            proc.wait(timeout=5)


def test_cancel_returns_false_for_finished_job(tmp_path: Path) -> None:
    journal = Journal(tmp_path / "opsboard.db")
    runner = JobRunner(repo_root=tmp_path, journal=journal)
    jid = journal.create(target="python", task="t", argv=["t"], worktree="/w", host_pid=1)
    journal.finish(jid, 0)
    assert runner.cancel(jid) is False


def test_cancel_all_stops_every_running_job(tmp_path: Path) -> None:
    procs = [_spawn_tree() for _ in range(3)]
    try:
        journal = Journal(tmp_path / "opsboard.db")
        runner = JobRunner(repo_root=tmp_path, journal=journal)
        for p in procs:
            _running_job(journal, p.pid)

        assert runner.cancel_all() == 3
        for p in procs:
            assert p.wait(timeout=5) is not None  # reap the killed leader
            assert _group_dead(p.pid), f"group {p.pid} survived cancel_all"
    finally:
        for p in procs:
            if p.poll() is None:
                p.kill()
                p.wait(timeout=5)


def test_cancelled_job_reaps_to_cancelled_status(tmp_path: Path) -> None:
    # After the tree is killed, the stale pid reaps to the CANCELLED bucket.
    proc = _spawn_tree()
    journal = Journal(tmp_path / "opsboard.db")
    runner = JobRunner(repo_root=tmp_path, journal=journal)
    jid = _running_job(journal, proc.pid)
    runner.cancel(jid, grace=0.3)
    proc.wait(timeout=5)  # reap the killed leader so its pid is truly gone
    journal.reap_stale()
    job = journal.get(jid)
    assert job is not None
    assert job.status == CANCELLED
