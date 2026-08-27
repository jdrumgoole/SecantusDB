"""A run that loses an xdist worker must exit non-zero.

Observed 2026-08-26 on a contended box: a worker was SIGKILLed about two thirds
of the way through, xdist logged ``node down: Not properly terminated`` plus an
``INTERNALERROR``, and pytest then printed ``4093 passed`` and exited **0** --
against 6257 collected. Roughly 2100 tests never ran, and the run still looked
green. This pins the exit status to the worker-death record so that cannot
happen silently again.
"""

from __future__ import annotations

import os
import pathlib
import subprocess
import sys
import textwrap

import conftest
from conftest import _lost_test_report


def test_clean_run_reports_nothing() -> None:
    assert _lost_test_report(collected=100, executed=100, node_down=[]) is None


def test_clean_run_with_deselects_still_reports_nothing() -> None:
    """No worker died, so a collected/executed gap is not our business."""
    assert _lost_test_report(collected=100, executed=87, node_down=[]) is None


def test_worker_death_reports_the_gap() -> None:
    msg = _lost_test_report(
        collected=6257,
        executed=4129,
        node_down=["<WorkerController gw7>: 'Not properly terminated'"],
    )
    assert msg is not None
    assert "RUN INVALID" in msg
    assert "gw7" in msg
    assert "never ran: 2128" in msg
    assert "6257" in msg and "4129" in msg


def test_worker_death_reported_even_when_nothing_is_missing() -> None:
    """A worker dying after its last test still invalidates the run: xdist may
    have aborted the loop, and we cannot prove otherwise from in here."""
    msg = _lost_test_report(collected=10, executed=10, node_down=["gw0: boom"])
    assert msg is not None
    assert "never ran: 0" in msg


def test_missing_count_never_goes_negative() -> None:
    msg = _lost_test_report(collected=5, executed=9, node_down=["gw1: boom"])
    assert msg is not None
    assert "never ran: 0" in msg


class _FakeConfig:
    """A controller config -- workers are distinguished by ``workerinput``."""


class _FakeWorkerConfig:
    workerinput = {"workerid": "gw3"}


class _FakeSession:
    def __init__(self, config, collected: int) -> None:
        self.config = config
        self.testscollected = collected
        self.exitstatus = 0


def test_hook_fails_a_run_that_would_have_exited_zero(monkeypatch) -> None:
    """The actual regression: exit status must reflect the worker death.

    Note this is tested against the hook directly rather than through a real
    xdist run. A synthetic worker suicide does NOT reproduce the observed bug --
    xdist reports the crashed item and fails the run on its own. The 2026-08-26
    case only exited 0 because the controller *also* hit an ``INTERNALERROR``
    while redistributing work, which swallowed the status; that interleaving
    isn't reliably stageable, so the override is pinned at its own level.
    """
    monkeypatch.setattr(conftest, "_node_down", ["<WorkerController gw7>: 'gone'"])
    monkeypatch.setattr(conftest, "_seen_nodeids", {f"t{i}" for i in range(4129)})
    session = _FakeSession(_FakeConfig(), collected=6257)

    conftest.pytest_sessionfinish(session, exitstatus=0)

    assert session.exitstatus == 1, "a lost worker must not exit 0"


def test_hook_leaves_a_clean_run_alone(monkeypatch) -> None:
    monkeypatch.setattr(conftest, "_node_down", [])
    monkeypatch.setattr(conftest, "_seen_nodeids", {f"t{i}" for i in range(10)})
    session = _FakeSession(_FakeConfig(), collected=10)

    conftest.pytest_sessionfinish(session, exitstatus=0)

    assert session.exitstatus == 0


def test_hook_preserves_an_existing_failure_status(monkeypatch) -> None:
    """A real test failure already failed the run; don't overwrite its code."""
    monkeypatch.setattr(conftest, "_node_down", ["gw1: gone"])
    monkeypatch.setattr(conftest, "_seen_nodeids", {"t0"})
    session = _FakeSession(_FakeConfig(), collected=10)
    session.exitstatus = 2

    conftest.pytest_sessionfinish(session, exitstatus=2)

    assert session.exitstatus == 2


def test_hook_is_a_no_op_inside_a_worker(monkeypatch) -> None:
    """Workers have their own exit path; only the controller decides the run."""
    monkeypatch.setattr(conftest, "_node_down", ["gw1: gone"])
    session = _FakeSession(_FakeWorkerConfig(), collected=10)

    conftest.pytest_sessionfinish(session, exitstatus=0)

    assert session.exitstatus == 0


def test_real_worker_death_is_detected_end_to_end(tmp_path) -> None:
    """A genuine dead worker is detected and the run does not silently succeed.

    Two things this deliberately does NOT do.

    It does not assert the exit code proves the override: xdist independently
    fails this synthetic case, so a returncode assertion passes even with the
    fix removed (checked). The override is pinned against the hook above.

    It does not insist on the banner specifically. A post-crash run can wedge --
    that is exactly why ``conftest`` carries a stall watchdog -- and when it
    does, the watchdog kills the process before ``pytest_sessionfinish`` ever
    runs, so no banner is printed. The first version of this test asserted only
    on the banner with a 300s subprocess timeout, passed standalone, and then
    hung for the full 300s inside a loaded full-suite run, because the wedge
    path's default deadline is 1200s. Both outcomes prove the same thing -- the
    worker death was noticed -- so either is accepted, and the nested run's
    deadlines are bounded via env so it can never outlive our timeout.
    """
    (tmp_path / "conftest.py").write_text(
        (pathlib.Path(__file__).parent / "conftest.py").read_text()
    )
    (tmp_path / "test_victim.py").write_text(
        textwrap.dedent(
            """
            import os, time
            import pytest

            @pytest.mark.parametrize("i", range(60))
            def test_ok(i):
                # Take the whole worker down the way an OOM kill does: no
                # exception, no report, the process simply stops. os._exit
                # skips all cleanup and exists on Windows too -- signal.SIGKILL
                # does not, which is how this first went red in CI.
                if i == 30:
                    os._exit(1)
                time.sleep(0.01)
                assert True
            """
        )
    )
    env = dict(os.environ)
    # Don't let the OUTER run's xdist/pytest state leak into the nested one --
    # this test runs inside an xdist worker during a full-suite run, and that
    # inherited state is the difference between the nested run finishing in ~1s
    # standalone and wedging under load.
    for leaked in (
        "PYTEST_XDIST_WORKER",
        "PYTEST_XDIST_WORKER_COUNT",
        "PYTEST_XDIST_TESTRUNUID",
        "PYTEST_CURRENT_TEST",
        "PYTEST_ADDOPTS",
    ):
        env.pop(leaked, None)
    # Bound the nested run so a post-crash wedge self-terminates well inside our
    # subprocess timeout instead of sitting on conftest's 1200s default.
    env["SECANTUS_POST_CRASH_SECONDS"] = "20"
    env["SECANTUS_STALL_SECONDS"] = "60"
    proc = subprocess.run(
        [sys.executable, "-m", "pytest", "-q", "-p", "no:randomly", "-n", "4", str(tmp_path)],
        cwd=tmp_path,
        env=env,
        capture_output=True,
        text=True,
        timeout=240,
    )
    out = proc.stdout + proc.stderr
    assert proc.returncode != 0, f"a dead worker must not exit 0:\n{out[-3000:]}"
    detected = "RUN INVALID" in out or "post-crash overrun" in out
    assert detected, f"worker death went unreported:\n{out[-3000:]}"
