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

import pytest
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

    keep = python_tasks._PYTEST_TMP_KEEP
    expected = sorted(f"pytest-{n}" for n in range(6, 6 - keep, -1))
    assert reaped == 6 - keep, reaped
    assert freed > 0
    survivors = sorted(p.name for p in root.iterdir())
    assert survivors == expected, survivors


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


def _symlink_or_skip(link, target) -> None:
    """Create ``link`` -> ``target``, or skip where the OS will not let this
    user create one: Windows without Developer Mode or admin rights refuses
    with ERROR_PRIVILEGE_NOT_HELD (1314). CI's Windows runners are admin, so
    the test still runs there."""
    try:
        link.symlink_to(target)
    except OSError as exc:
        if getattr(exc, "winerror", None) == 1314:
            pytest.skip("creating a symlink needs a privilege this Windows user lacks")
        raise


def test_symlinks_and_foreign_names_are_left_alone(tmp_path: Path) -> None:
    root = tmp_path / f"pytest-of-{__import__('getpass').getuser()}"
    root.mkdir()
    for n in range(1, 6):
        _make_run(root, n, lock_pid=None)
    _symlink_or_skip(root / "pytest-current", root / "pytest-5")
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

    keep = python_tasks._PYTEST_TMP_KEEP
    expected = sorted(f"pytest-{n}" for n in range(6, 6 - keep, -1))
    assert reaped == 6 - keep, reaped
    assert freed == 0, "measure=False must not walk the trees"
    survivors = sorted(p.name for p in root.iterdir())
    assert survivors == expected, survivors


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


# --------------------------------------------------------------- probe stores


def _make_probe_store(base: Path, pid: int, tag: str = "abcd1234") -> Path:
    d = base / f"secantus-probe-{pid}-{tag}"
    d.mkdir(parents=True)
    (d / "WiredTiger.wt").write_bytes(b"x" * 1024)
    return d


def test_probe_store_of_a_dead_pid_is_reaped(tmp_path: Path) -> None:
    """The backstop for a probe that died holding its store open.

    ``probe_store``'s atexit delete cannot cover this: on Windows an open file
    cannot be deleted at all, so a probe killed while its server is up leaves
    the home behind with WiredTiger still holding it.
    """
    dead = _make_probe_store(tmp_path, 999_999_999)

    reaped, freed = python_tasks._sweep_stale_probe_tmp(str(tmp_path))

    assert reaped == 1, reaped
    assert freed > 0
    assert not dead.exists()


def test_probe_store_of_a_live_pid_is_never_reaped(tmp_path: Path) -> None:
    """A probe running right now must keep its database."""
    live = _make_probe_store(tmp_path, os.getpid())

    reaped, _ = python_tasks._sweep_stale_probe_tmp(str(tmp_path))

    assert reaped == 0
    assert live.exists(), "swept the store of a running probe"


def test_probe_sweep_leaves_foreign_names_alone(tmp_path: Path) -> None:
    """The system tempdir is shared; only our own prefix is ours to delete."""
    (tmp_path / "tmpsomething").mkdir()
    (tmp_path / "secantus-pymongo-gauge-xyz").mkdir()
    # A malformed name -- no PID where one belongs -- is ambiguous evidence,
    # so it fails toward keeping the data like every other check here.
    (tmp_path / "secantus-probe-notapid-xx").mkdir()

    reaped, _ = python_tasks._sweep_stale_probe_tmp(str(tmp_path))

    assert reaped == 0
    assert (tmp_path / "tmpsomething").exists()
    assert (tmp_path / "secantus-pymongo-gauge-xyz").exists()
    assert (tmp_path / "secantus-probe-notapid-xx").exists()


def test_session_finish_reaps_too(monkeypatch, tmp_path: Path) -> None:
    """Cleanup on the way OUT, not only on the way in.

    Reaping only at session start leaves the last run's WiredTiger homes on
    disk for as long as nobody runs pytest again -- which is how this box
    reached 50 MB free with the start-of-session sweep working perfectly.

    The reap is folded into the EXISTING ``pytest_sessionfinish`` (the
    lost-worker exit-status hook) rather than added as a second definition of
    the same name: a module can only have one, and the later one silently wins.
    """
    import tests.conftest as ct

    calls: list[str] = []
    monkeypatch.setattr(
        python_tasks,
        "_sweep_stale_pytest_tmp",
        lambda *a, **k: (calls.append("pytest"), (0, 0))[1],
    )
    monkeypatch.setattr(
        python_tasks,
        "_sweep_stale_probe_tmp",
        lambda *a, **k: (calls.append("probe"), (0, 0))[1],
    )
    monkeypatch.setattr(ct, "_lost_test_report", lambda *a, **k: None)

    class _Config:
        pass

    class _Session:
        config = _Config()
        testscollected = 0

    ct.pytest_sessionfinish(_Session(), 0)

    assert calls == ["pytest", "probe"], calls


def test_only_one_session_finish_hook_is_defined() -> None:
    """A second ``def pytest_sessionfinish`` would silently disable the first.

    This was written as a separate hook first, and Python quietly kept only the
    later definition -- the reap never ran, and nothing failed to say so.
    """
    source = (Path(__file__).parent / "conftest.py").read_text(encoding="utf-8")

    assert source.count("def pytest_sessionfinish(") == 1
    assert source.count("def pytest_sessionstart(") == 1


def test_probe_prefix_matches_the_probe_helper() -> None:
    """The sweeper's prefix and the probe helper's must not drift apart.

    ``python_tasks`` deliberately duplicates the constant instead of importing
    ``tools/probes/_servers`` -- that module imports ``pymongo`` at module
    scope, and this sweep has to work where no probe dependency is installed.
    A duplicated constant needs a test or it silently stops matching, and the
    failure mode is a sweep that reaps nothing while looking healthy.
    """
    import sys

    sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "tools" / "probes"))
    try:
        import _servers
    except ImportError:  # pragma: no cover - pymongo absent (slim CI env)
        pytest.skip("tools/probes/_servers needs pymongo")

    assert _servers.PROBE_TMP_PREFIX == python_tasks._PROBE_TMP_PREFIX


# ------------------------------------------------- the store, not just the name


def test_a_live_wiredtiger_home_is_never_swept(tmp_path: Path) -> None:
    """The regression that matters: a RUNNING database must keep its files.

    On 2026-09-22 this sweep deleted a live mongod's dbpath. The directory had
    been named by hand with a shell's ``$$`` instead of the server's pid, so
    the name pointed at a long-dead shell; the sweep believed it and rmtree'd
    the store, and mongod died on a fatal WiredTiger assertion. A name is
    written by whoever made the directory and can simply be wrong -- so the
    store itself is asked, and a dead pid is no longer sufficient grounds.
    """
    from secantus import SecantusDBServer

    # A real WiredTiger home, open, named for a pid that is definitely gone.
    home = tmp_path / f"{python_tasks._PROBE_TMP_PREFIX}999999999-live"
    home.mkdir()
    server = SecantusDBServer(port=0, storage_path=str(home))
    server.start()
    try:
        assert python_tasks._wt_home_in_use(str(home)), "an open WT home reads as free"

        reaped, _ = python_tasks._sweep_stale_probe_tmp(str(tmp_path))

        assert reaped == 0, "swept a live database"
        assert (home / "WiredTiger.wt").exists(), "deleted a live store's files"
    finally:
        server.stop()


def test_a_closed_wiredtiger_home_is_still_reaped(tmp_path: Path) -> None:
    """The guard must not turn the sweep into a no-op.

    A store whose server has stopped is exactly what this reclaims, and the
    whole point of the fix is that it keeps doing so.
    """
    from secantus import SecantusDBServer

    home = tmp_path / f"{python_tasks._PROBE_TMP_PREFIX}999999999-done"
    home.mkdir()
    server = SecantusDBServer(port=0, storage_path=str(home))
    server.start()
    server.stop()

    assert not python_tasks._wt_home_in_use(str(home))

    reaped, _ = python_tasks._sweep_stale_probe_tmp(str(tmp_path))

    assert reaped == 1
    assert not home.exists()


def test_wt_home_in_use_is_false_without_a_lock_file(tmp_path: Path) -> None:
    """A plain directory is not a WiredTiger home and blocks nothing."""
    d = tmp_path / "plain"
    d.mkdir()
    (d / "notes.txt").write_text("x")

    assert python_tasks._wt_home_in_use(str(d)) is False
