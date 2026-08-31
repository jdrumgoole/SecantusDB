"""The sweep of abandoned pytest temp trees.

Two callers: ``invoke clean`` (explicit, reports the bytes it freed) and
``tests/conftest.py``'s ``pytest_sessionstart`` (automatic, skips the sizing
walk). The automatic one is what stops the backlog returning -- ``invoke
clean`` could always fix this, but only when somebody remembered to run it.


This suite pins ``tmp_path_retention_policy = "all"`` (deleting a passed
test's ``tmp_path`` mid-session races WiredTiger into ``WT_PANIC`` — see
``tests/conftest.py``), so each run leaves its per-test WiredTiger databases
behind and relies on pytest's own numbered-dir cleanup to reclaim them. That
janitor stalls for a full 3-day ``LOCK_TIMEOUT`` whenever a run dies without
running its atexit hooks, which is how one box reached 241 dirs / 391 GiB.
The sweeper uses the PID pytest writes into each ``.lock`` to decide liveness
now instead of in three days.
"""

from __future__ import annotations

import os
from pathlib import Path

import python_tasks


def _make_run(root: Path, number: int, *, lock_pid: int | None) -> Path:
    d = root / f"pytest-{number}"
    (d / "test_something0").mkdir(parents=True)
    (d / "test_something0" / "WiredTiger.wt").write_bytes(b"x" * 1024)
    if lock_pid is not None:
        (d / ".lock").write_text(str(lock_pid))
    return d


def test_sweeps_abandoned_runs_but_keeps_the_newest(tmp_path: Path) -> None:
    root = tmp_path / f"pytest-of-{__import__('getpass').getuser()}"
    root.mkdir()
    runs = [_make_run(root, n, lock_pid=None) for n in range(1, 7)]
    # Make mtimes strictly ordered so "newest" is unambiguous.
    for i, d in enumerate(runs):
        os.utime(d, (1_000_000 + i, 1_000_000 + i))

    reaped, freed = python_tasks._sweep_stale_pytest_tmp(str(tmp_path))

    assert reaped == 3, reaped
    assert freed > 0
    survivors = sorted(p.name for p in root.iterdir())
    assert survivors == ["pytest-4", "pytest-5", "pytest-6"], survivors


def test_a_live_owner_is_never_swept(tmp_path: Path) -> None:
    """A dir whose lock PID is still running belongs to a live session."""
    root = tmp_path / f"pytest-of-{__import__('getpass').getuser()}"
    root.mkdir()
    for n in range(1, 6):
        _make_run(root, n, lock_pid=None)
    live = _make_run(root, 0, lock_pid=os.getpid())  # this very process
    os.utime(live, (1, 1))  # oldest, so retention alone would not save it

    python_tasks._sweep_stale_pytest_tmp(str(tmp_path))

    assert live.exists(), "swept a directory whose owning process is alive"


def test_a_stale_lock_does_not_protect_a_dead_run(tmp_path: Path) -> None:
    """The whole point: a dead PID's lock must not buy a 3-day reprieve."""
    root = tmp_path / f"pytest-of-{__import__('getpass').getuser()}"
    root.mkdir()
    for n in range(1, 5):
        _make_run(root, n, lock_pid=None)
    dead_pid = 999_999_999  # not a running process
    stale = _make_run(root, 0, lock_pid=dead_pid)
    os.utime(stale, (1, 1))

    python_tasks._sweep_stale_pytest_tmp(str(tmp_path))

    assert not stale.exists(), "a stale lock still blocked the sweep"


def test_unreadable_lock_is_treated_as_alive(tmp_path: Path) -> None:
    """Ambiguous evidence must fail toward keeping the data."""
    root = tmp_path / f"pytest-of-{__import__('getpass').getuser()}"
    root.mkdir()
    for n in range(1, 5):
        _make_run(root, n, lock_pid=None)
    weird = _make_run(root, 0, lock_pid=None)
    (weird / ".lock").write_text("not-a-pid")
    os.utime(weird, (1, 1))

    python_tasks._sweep_stale_pytest_tmp(str(tmp_path))

    assert weird.exists()


def test_symlinks_and_foreign_names_are_left_alone(tmp_path: Path) -> None:
    root = tmp_path / f"pytest-of-{__import__('getpass').getuser()}"
    root.mkdir()
    for n in range(1, 6):
        _make_run(root, n, lock_pid=None)
    (root / "pytest-current").symlink_to(root / "pytest-5")
    keep_me = root / "not-a-pytest-dir"
    keep_me.mkdir()

    python_tasks._sweep_stale_pytest_tmp(str(tmp_path))

    assert (root / "pytest-current").is_symlink()
    assert keep_me.exists()


def test_missing_root_is_a_noop(tmp_path: Path) -> None:
    assert python_tasks._sweep_stale_pytest_tmp(str(tmp_path / "nope")) == (0, 0)


def test_measure_false_skips_sizing_but_still_reaps(tmp_path: Path) -> None:
    """The session-start caller wants the deletion, not the byte count.

    Sizing walks every file to produce ``invoke clean``'s summary line, which
    doubles the I/O on a big backlog -- and the backlog is exactly when the
    automatic sweep fires.
    """
    root = tmp_path / f"pytest-of-{__import__('getpass').getuser()}"
    root.mkdir()
    runs = [_make_run(root, n, lock_pid=None) for n in range(1, 7)]
    for i, d in enumerate(runs):
        os.utime(d, (1_000_000 + i, 1_000_000 + i))

    reaped, freed = python_tasks._sweep_stale_pytest_tmp(str(tmp_path), measure=False)

    assert reaped == 3, reaped
    assert freed == 0, "measure=False must not walk the trees"
    survivors = sorted(p.name for p in root.iterdir())
    assert survivors == ["pytest-4", "pytest-5", "pytest-6"], survivors


def test_session_start_reaper_is_controller_only(monkeypatch) -> None:
    """xdist workers must not each redo the sweep and race one another.

    Twelve workers all reaping the same tree would have them deleting each
    other's candidates mid-``rmtree``.
    """
    import conftest

    calls: list[str] = []
    monkeypatch.setattr(
        python_tasks, "_sweep_stale_pytest_tmp", lambda *a, **k: calls.append("swept") or (0, 0)
    )
    monkeypatch.delenv("SECANTUS_NO_TMP_REAP", raising=False)

    class _Worker:
        workerinput = {"workerid": "gw3"}

    class _Controller:
        pass

    conftest._reap_abandoned_pytest_tmp(_Worker())
    assert calls == [], "an xdist worker ran the sweep"

    conftest._reap_abandoned_pytest_tmp(_Controller())
    assert calls == ["swept"], "the controller did not run the sweep"


def test_session_start_reaper_never_raises(monkeypatch, tmp_path: Path) -> None:
    """Housekeeping must never fail a test run, whatever goes wrong."""
    import conftest

    class _Cfg:
        pass  # no workerinput -> the controller path

    def _boom(*a, **k):
        raise RuntimeError("disk on fire")

    monkeypatch.setattr(python_tasks, "_sweep_stale_pytest_tmp", _boom)
    conftest._reap_abandoned_pytest_tmp(_Cfg())  # must not raise


def test_session_start_reaper_respects_the_opt_out(monkeypatch) -> None:
    import conftest

    class _Cfg:
        pass

    calls: list[int] = []
    monkeypatch.setenv("SECANTUS_NO_TMP_REAP", "1")
    monkeypatch.setattr(
        python_tasks, "_sweep_stale_pytest_tmp", lambda *a, **k: calls.append(1) or (0, 0)
    )
    conftest._reap_abandoned_pytest_tmp(_Cfg())
    assert calls == [], "opt-out did not prevent the sweep"
