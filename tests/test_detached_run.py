"""``scripts/detached_run.py`` must outlive a kill of the launching shell.

A supervisor that reaps the shell it spawned signals the whole process
group, which is what killed three multi-minute suite runs in one session.
The helper exists so the real work sits in its OWN process group; the test
that matters is therefore the one that signals the launcher's group and
asserts the child is still there.
"""

from __future__ import annotations

import json
import os
import signal
import subprocess
import sys
import time
from pathlib import Path

import pytest

SCRIPT = Path(__file__).resolve().parent.parent / "scripts" / "detached_run.py"


def _run(state_dir: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(SCRIPT), "--state-dir", str(state_dir), *args],
        capture_output=True,
        text=True,
    )


def _wait_for(predicate, timeout: float = 20.0, interval: float = 0.2) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return True
        time.sleep(interval)
    return False


def _alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    return True


@pytest.mark.skipif(os.name != "posix", reason="process groups are POSIX")
def test_child_runs_in_its_own_process_group(tmp_path: Path) -> None:
    state = tmp_path / "runs"
    out = _run(state, "start", "--name", "pg", "--", "sleep", "5")
    assert out.returncode == 0, out.stderr
    pid = json.loads((state / "pg.json").read_text())["pid"]
    try:
        assert os.getpgid(pid) != os.getpgid(os.getpid())
        assert os.getpgid(pid) == pid, "the child should lead its own group"
    finally:
        _run(state, "stop", "--name", "pg")


@pytest.mark.skipif(os.name != "posix", reason="process groups are POSIX")
def test_child_survives_a_kill_of_the_launching_group(tmp_path: Path) -> None:
    """The regression this script exists for."""
    state = tmp_path / "runs"
    marker = tmp_path / "finished"
    out = _run(
        state,
        "start",
        "--name",
        "survivor",
        "--",
        "/bin/sh",
        "-c",
        f"sleep 3; echo done > {marker}",
    )
    assert out.returncode == 0, out.stderr
    pid = json.loads((state / "survivor.json").read_text())["pid"]

    # Signal OUR whole process group, the way a reaped shell is killed.
    # The child is in a different group, so it must be untouched.
    os.killpg(os.getpgid(os.getpid()), signal.SIGCONT)
    os.kill(pid, 0)  # raises if the signal reached it

    assert _wait_for(marker.exists), "detached child did not finish"
    assert marker.read_text().strip() == "done"
    assert _wait_for(lambda: not _alive(pid))
    status = _run(state, "status", "--name", "survivor")
    assert "finished rc=0" in status.stdout, status.stdout


@pytest.mark.skipif(os.name != "posix", reason="process groups are POSIX")
def test_failure_exit_code_is_recorded(tmp_path: Path) -> None:
    state = tmp_path / "runs"
    assert _run(state, "start", "--name", "boom", "--", "/bin/sh", "-c", "exit 7").returncode == 0
    wait = _run(state, "wait", "--name", "boom", "--timeout", "20", "--interval", "0.2")
    assert wait.returncode == 1, wait.stdout
    assert "rc=7" in wait.stdout


@pytest.mark.skipif(os.name != "posix", reason="process groups are POSIX")
def test_a_second_start_under_a_live_name_is_refused(tmp_path: Path) -> None:
    state = tmp_path / "runs"
    assert _run(state, "start", "--name", "dup", "--", "sleep", "5").returncode == 0
    try:
        again = _run(state, "start", "--name", "dup", "--", "sleep", "5")
        assert again.returncode != 0
        assert "still running" in again.stderr
    finally:
        _run(state, "stop", "--name", "dup")
