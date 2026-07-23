"""Tests for the shared job runner + journal (``secantus.jobkit``).

Drives the real sqlite journal and the real pty-tee runner against a trivial
child command (via the ``SECANTUS_OPSBOARD_INVOKE`` override) — no mock, no
dependency on ``uv``/``invoke`` being resolvable in the test env.
"""

from __future__ import annotations

import sys
import time
from pathlib import Path

import pytest

from secantus.jobkit import (
    CANCELLED,
    FAILED,
    PASSED,
    RUNNING,
    Journal,
    infer_target,
    run_tracked,
    status_for_exit,
)

# A stand-in for `uv run ... python -m invoke`: prints its argv and exits with
# the trailing integer (so a test can force a pass/fail code deterministically).
_FAKE_INVOKE = (
    "import sys; "
    "args = sys.argv[1:]; "
    "print('CHILD-RAN', *args); "
    "code = int(args[-1]) if args and args[-1].lstrip('-').isdigit() else 0; "
    "sys.exit(code)"
)


@pytest.fixture
def journal(tmp_path: Path) -> Journal:
    return Journal(tmp_path / "opsboard.db")


@pytest.fixture(autouse=True)
def _isolate(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("SECANTUS_OPSBOARD_DB", str(tmp_path / "opsboard.db"))
    monkeypatch.setenv("SECANTUS_OPSBOARD_LOGS", str(tmp_path / "logs"))
    monkeypatch.setenv("SECANTUS_OPSBOARD_INVOKE", f"{sys.executable} -c {_FAKE_INVOKE!r}")


# --------------------------------------------------------------------------- #
# Journal
# --------------------------------------------------------------------------- #


def test_journal_create_get_finish(journal: Journal) -> None:
    jid = journal.create(target="python", task="test", argv=["test"], worktree="/w", host_pid=123)
    job = journal.get(jid)
    assert job is not None
    assert job.status == RUNNING
    assert job.task == "test"
    assert job.argv == ["test"]
    assert job.exit_code is None
    assert job.running is True

    journal.finish(jid, 0)
    job = journal.get(jid)
    assert job is not None
    assert job.status == PASSED
    assert job.exit_code == 0
    assert job.running is False
    assert job.ended_at is not None


def test_journal_pagination_is_newest_first_and_cursored(journal: Journal) -> None:
    ids = [
        journal.create(target="python", task=f"t{i}", argv=[f"t{i}"], worktree="/w", host_pid=1)
        for i in range(5)
    ]

    page1, cursor1 = journal.list(limit=2)
    assert [j.id for j in page1] == [ids[4], ids[3]]  # newest first
    assert cursor1 == ids[3]

    page2, cursor2 = journal.list(limit=2, before_id=cursor1)
    assert [j.id for j in page2] == [ids[2], ids[1]]
    assert cursor2 == ids[1]

    page3, cursor3 = journal.list(limit=2, before_id=cursor2)
    assert [j.id for j in page3] == [ids[0]]
    assert cursor3 is None  # last page → no further cursor


def test_journal_running_filter(journal: Journal) -> None:
    a = journal.create(target="python", task="a", argv=["a"], worktree="/w", host_pid=1)
    b = journal.create(target="rust", task="b", argv=["b"], worktree="/w", host_pid=1)
    journal.finish(a, 0)
    running = journal.running()
    assert [j.id for j in running] == [b]


def test_journal_reap_stale_marks_dead_pid_cancelled(journal: Journal) -> None:
    # A pid that cannot be alive (0 is never a real user process here).
    dead = journal.create(
        target="python", task="x", argv=["x"], worktree="/w", host_pid=999_999_999
    )
    reaped = journal.reap_stale()
    assert reaped == 1
    job = journal.get(dead)
    assert job is not None
    assert job.status == CANCELLED


# --------------------------------------------------------------------------- #
# Classification helpers
# --------------------------------------------------------------------------- #


@pytest.mark.parametrize(
    ("task", "argv", "expected"),
    [
        ("test", [], "python"),
        ("py-gate", [], "python"),
        ("validate", [], "python"),
        ("validate", ["validate", "--server", "rust"], "rust"),
        ("rust-gate", [], "rust"),
        ("rust-binary-build", [], "rust"),
        ("validate-psycopg", [], "pg"),
        ("validate-slt", [], "pg"),
        ("something-else", [], "other"),
    ],
)
def test_infer_target(task: str, argv: list[str], expected: str) -> None:
    assert infer_target(task, argv) == expected


def test_status_for_exit() -> None:
    assert status_for_exit(None) == RUNNING
    assert status_for_exit(0) == PASSED
    assert status_for_exit(2) == FAILED
    assert status_for_exit(130) == CANCELLED  # SIGINT
    assert status_for_exit(143) == CANCELLED  # SIGTERM


# --------------------------------------------------------------------------- #
# The tracked runner (real subprocess under a pty)
# --------------------------------------------------------------------------- #


def test_run_tracked_records_pass_and_tees_log(tmp_path: Path) -> None:
    journal = Journal(tmp_path / "opsboard.db")
    code = run_tracked(["greet", "0"], journal=journal, echo=False)
    assert code == 0

    jobs, _ = journal.list()
    assert len(jobs) == 1
    job = jobs[0]
    assert job.status == PASSED
    assert job.exit_code == 0
    assert job.task == "greet"
    assert job.target == "other"
    # The child's output was teed to the per-job logfile.
    assert job.log_path is not None
    log = Path(job.log_path).read_text()
    assert "CHILD-RAN" in log
    assert "greet" in log


def test_run_tracked_records_failure_exit_code(tmp_path: Path) -> None:
    journal = Journal(tmp_path / "opsboard.db")
    code = run_tracked(["boom", "7"], journal=journal, echo=False)
    assert code == 7
    jobs, _ = journal.list()
    assert jobs[0].status == FAILED
    assert jobs[0].exit_code == 7


_TTY_PROBE = """\
import os, sys
ok = True
if os.isatty(0):
    try:
        os.tcgetpgrp(0)
    except OSError:
        ok = False  # stdin is a broken pty slave — the regression we guard
print("STDIN_OK", ok, "STDOUT_TTY", os.isatty(1))
sys.exit(0 if ok else 3)
"""


@pytest.mark.skipif(
    sys.platform == "win32", reason="pty is POSIX-only (Windows uses the pipe fallback)"
)
def test_run_tracked_does_not_give_child_a_broken_pty_stdin(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    # Regression: stdin must be inherited, not the pty slave. A pty-slave stdin
    # made downstream `invoke` tasks crash in os.tcgetpgrp() under CI. stdout
    # must still be a pty (colours). The probe exits 3 if stdin is a broken pty.
    probe = tmp_path / "tty_probe.py"
    probe.write_text(_TTY_PROBE)
    monkeypatch.setenv("SECANTUS_OPSBOARD_INVOKE", f"{sys.executable} {probe}")
    journal = Journal(tmp_path / "opsboard.db")
    code = run_tracked(["probe"], journal=journal, echo=False)
    assert code == 0  # would be 3 if stdin were the pty slave
    job = journal.list()[0][0]
    assert job.status == PASSED
    log = Path(job.log_path).read_text()
    assert "STDOUT_TTY True" in log  # pty still drives stdout


def test_run_tracked_pipe_fallback_when_no_pty(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    # Force the no-pty path (what Windows uses) and confirm it still journals +
    # tees the child's output.
    from secantus.jobkit import _core

    monkeypatch.setattr(_core, "_pty", None)
    journal = Journal(tmp_path / "opsboard.db")
    code = run_tracked(["pipe", "0"], journal=journal, echo=False)
    assert code == 0
    job = journal.list()[0][0]
    assert job.status == PASSED
    assert "CHILD-RAN" in Path(job.log_path).read_text()


def test_run_tracked_sets_started_and_ended(tmp_path: Path) -> None:
    journal = Journal(tmp_path / "opsboard.db")
    before = time.time()
    run_tracked(["quick", "0"], journal=journal, echo=False)
    jobs, _ = journal.list()
    job = jobs[0]
    assert job.started_at >= before
    assert job.ended_at is not None
    assert job.duration >= 0.0
