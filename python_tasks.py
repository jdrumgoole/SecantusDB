"""Invoke tasks for the Python side of SecantusDB (the `secantus` package).

Imported by the root ``tasks.py`` (``from python_tasks import *``) so the
Python-server dev workflow — sync / test / test-one / perf / lint / fmt / serve /
docs / docs-serve / clean, plus ``py-gate`` (the full pre-commit sequence) and
``py-ship`` (gate → commit → push) — lives in one place, mirroring how the
``rust-*`` tasks live in ``rust_tasks.py``. ``import *`` brings the Task objects
into the root namespace for invoke discovery, so ``invoke test`` / ``invoke lint``
etc. are unchanged.

Scope: the pure-Python server (`src/secantus/**`) and its docs/tests. The
driver-conformance gauges (`validate-*`), the release pipeline
(`release-*`), and the bench/chaos harnesses stay in `tasks.py` — they're
cross-cutting (they target either server) rather than Python-server dev tasks.
"""

from __future__ import annotations

import getpass
import os
import re
import shlex
import shutil

from invoke.context import Context
from invoke.tasks import task

# Ruff is a pure-Python static check that needs neither the compiled `secantus`
# extension nor a synced project env, so `lint` / `fmt` run it through `uvx`
# (an ephemeral, cached tool env) instead of `uv run` — the latter would try to
# sync/build the project (a multi-minute WiredTiger compile) and fails outright
# in a fresh git worktree where `vendor/wiredtiger` isn't populated. Pin the
# version so local output matches CI's `ruff format --check`; keep this in sync
# with the `ruff==` pin in pyproject.toml's [project.optional-dependencies] dev.
_RUFF = "ruff@0.15.20"

# `docs` builds WT-free via `uv run --no-project` (autodoc imports `secantus`
# from src/ with `wiredtiger` mocked — see docs/conf.py), so it needs no project
# build and works in a bare git worktree. This overlay provides the Sphinx
# toolchain plus secantus's pure-Python runtime deps that autodoc must import;
# keep it in sync with pyproject's [project] dependencies (minus the compiled
# wiredtiger) and the docs entries of the dev extra.
_DOCS_DEPS = (
    "--with sphinx --with myst-parser --with furo "
    "--with pymongo --with shapely --with s2sphere --with python-dateutil"
)

# The known local-only failing test: a feature worktree's Rust crates link the
# /tmp WiredTiger build while the project wheel links its own, so this
# cross-server restore isn't byte-compatible locally (green in CI). Deselected
# from py-gate by default; pass --deselect "" to re-include it.
_LOCAL_DESELECT = "tests/test_rust_pitr_cross_server.py::test_python_restores_rust_data_to_a_mark"


@task
def sync(c: Context) -> None:
    c.run("uv sync --extra dev", pty=True)


@task
def test(c: Context, k: str = "", verbose: bool = False) -> None:
    cmd = "uv run python -m pytest"
    if verbose:
        cmd += " -v"
    if k:
        # shlex.quote — `f"{k!r}"` uses Python repr() which wraps in
        # single quotes but doesn't escape embedded single quotes,
        # leaving a shell-injection hole on a CLI-supplied filter.
        cmd += f" -k {shlex.quote(k)}"
    c.run(cmd, pty=True)


@task(name="test-one")
def test_one(c: Context, nodeid: str) -> None:
    # `-n0 -o addopts=` runs serially: `-p no:xdist` fails because the
    # project's addopts still injects `-n auto --dist=loadgroup`, which xdist
    # then can't parse once its plugin is disabled. Clearing addopts and forcing
    # zero workers is the reliable single-test form.
    c.run(
        "uv run --no-sync python -m pytest -n0 -o addopts= -p no:cacheprovider "
        f"{shlex.quote(nodeid)}",
        pty=True,
    )


@task(name="perf")
def perf_task(c: Context) -> None:
    """Run the performance regression suite (serially, no xdist).

    Benchmarks fight for CPU under parallel workers, amplifying noise to
    the point that the gate becomes flappy — so this task forces serial
    execution and explicitly opts in to the ``perf`` marker excluded
    from the default ``invoke test``. Median time per workload is
    asserted against a hard upper bound calibrated for ``:memory:``
    storage on a quiet 2024-era arm64 mac. Lower the bounds in
    ``tests/test_perf_regression.py`` when an optimisation moves the
    floor.
    """
    c.run(
        "uv run python -m pytest -p no:xdist "
        "-o addopts= -m perf "
        "--benchmark-columns=min,median,max -v "
        "tests/test_perf_regression.py",
        pty=True,
    )


@task
def lint(c: Context) -> None:
    c.run(f"uvx {_RUFF} check src tests", pty=True)
    c.run(f"uvx {_RUFF} format --check src tests", pty=True)


@task
def fmt(c: Context) -> None:
    c.run(f"uvx {_RUFF} format src tests", pty=True)
    c.run(f"uvx {_RUFF} check --fix src tests", pty=True)


@task
def serve(c: Context, host: str = "127.0.0.1", port: int = 27017) -> None:
    c.run(
        f"uv run python -m secantus --host {shlex.quote(host)} --port {int(port)}",
        pty=True,
    )


@task
def docs(c: Context, builder: str = "html", clean: bool = False) -> None:
    # Build WT-free with `--no-project`: autodoc imports `secantus` from src/
    # (conf.py adds it to sys.path) with the compiled `wiredtiger` mocked
    # (conf.py autodoc_mock_imports), so no scikit-build-core / WiredTiger
    # compile runs. This is what lets `invoke docs` work in a bare git worktree
    # that never ran a project build. `_DOCS_DEPS` overlays the doc toolchain
    # plus secantus's pure-Python runtime deps.
    if clean:
        c.run("rm -rf docs/_build", pty=True)
    qb = shlex.quote(builder)
    c.run(
        f"uv run --no-project {_DOCS_DEPS} "
        f"sphinx-build -W --keep-going -b {qb} docs docs/_build/{qb}",
        pty=True,
    )


@task(name="docs-rust")
def docs_rust(c: Context, builder: str = "html", clean: bool = False) -> None:
    """Build the Rust-server docs tree (docs-rust/) with warnings-as-errors.

    Pure-markdown Sphinx tree (no autodoc, no secantus import) — the version
    is regex-read from crates/secantusdb/Cargo.toml, so this works in any
    bare worktree with no build at all.
    """
    if clean:
        c.run("rm -rf docs-rust/_build", pty=True)
    qb = shlex.quote(builder)
    c.run(
        f"uv run --no-project {_DOCS_DEPS} "
        f"sphinx-build -W --keep-going -b {qb} docs-rust docs-rust/_build/{qb}",
        pty=True,
    )


@task(name="docs-serve")
def docs_serve(c: Context, port: int = 8000) -> None:
    docs(c)
    c.run(
        f"uv run --no-sync python -m http.server {port} --directory docs/_build/html",
        pty=True,
    )


@task
def clean(c: Context) -> None:
    c.run(
        "rm -rf build dist *.egg-info .pytest_cache .ruff_cache .coverage htmlcov docs/_build",
        pty=True,
    )
    # Sweep leaked gauge tempdirs. Aborted runs of ``invoke validate-*``
    # leave ``secantus-<driver>-gauge-XXXXXX`` directories under the
    # system tempdir (``/var/folders/.../T`` on macOS, ``/tmp`` on
    # Linux). Each holds a WiredTiger store — tens of MB at minimum.
    # Reap anything older than an hour so an active gauge isn't
    # interrupted.
    import glob
    import os
    import shutil
    import tempfile
    import time

    cutoff = time.time() - 3600.0
    base = tempfile.gettempdir()
    candidates = glob.glob(os.path.join(base, "secantus-*-gauge-*"))
    swept = 0
    for path in candidates:
        try:
            if os.stat(path).st_mtime < cutoff:
                shutil.rmtree(path, ignore_errors=True)
                swept += 1
        except FileNotFoundError:
            continue
    if swept:
        print(f"clean: swept {swept} stale gauge tempdir(s) older than 1h under {base}")

    reaped, freed = _sweep_stale_pytest_tmp(base)
    if reaped:
        print(
            f"clean: reaped {reaped} abandoned pytest tempdir(s) "
            f"({freed / 1024**3:.1f} GiB) under {base}"
        )


#: How many numbered pytest dirs to keep, mirroring pytest's own retention.
_PYTEST_TMP_KEEP = 3


def _sweep_stale_pytest_tmp(base: str) -> tuple[int, int]:
    """Delete abandoned ``pytest-of-<user>/pytest-NNNN`` trees; return
    ``(count, bytes_freed)``.

    This suite pins ``tmp_path_retention_policy = "all"`` on purpose — deleting
    a passed test's ``tmp_path`` mid-session races WiredTiger's background
    threads into ``WT_PANIC`` (see ``tests/conftest.py``) — so every run leaves
    its per-test WiredTiger databases behind, ~1.7 GiB a run. Reclaiming them
    is pytest's job: it keeps the newest few and ``rmtree``s the rest.

    That janitor stops working the moment a run dies abnormally. pytest writes
    the owning PID into a ``.lock`` beside each dir and removes it in an
    ``atexit`` hook, so a run killed by SIGKILL — or by this suite's own stall
    watchdog, which calls ``os._exit(70)`` — leaves the lock behind, and pytest
    then treats that dir as live for a full ``LOCK_TIMEOUT`` (3 days). The
    backlog compounds: the bigger it gets the longer the exit-time cleanup
    takes, so more runs are killed mid-cleanup, each leaving another stale
    lock. One dev box reached 241 dirs / 391 GiB that way, and every pytest
    invocation on it paid an unbounded exit-time ``rmtree``.

    A lock's PID makes liveness decidable now instead of in three days: if the
    process is gone, the lock is stale and the tree is garbage. Dirs whose
    owner is still running are left strictly alone, as are the newest
    ``_PYTEST_TMP_KEEP``.
    """
    root = os.path.join(base, f"pytest-of-{getpass.getuser()}")
    if not os.path.isdir(root):
        return (0, 0)
    numbered = [
        os.path.join(root, name)
        for name in os.listdir(root)
        if re.fullmatch(r"pytest-\d+", name) and os.path.isdir(os.path.join(root, name))
    ]
    # Never touch symlinks (``pytest-current``) or the root itself.
    numbered = [p for p in numbered if not os.path.islink(p)]
    numbered.sort(key=lambda p: os.stat(p).st_mtime, reverse=True)

    reaped = freed = 0
    for path in numbered[_PYTEST_TMP_KEEP:]:
        if _pytest_tmp_owner_alive(path):
            continue
        try:
            freed += _dir_size(path)
            shutil.rmtree(path, ignore_errors=True)
            reaped += 1
        except OSError:
            continue
    return (reaped, freed)


def _pytest_tmp_owner_alive(path: str) -> bool:
    """Whether the pytest run that owns ``path`` is still running.

    No lock file means nobody claimed it (or the owner exited cleanly and
    removed it) — garbage either way. A lock whose PID no longer exists is
    stale. Anything unreadable is treated as ALIVE: refusing to delete is the
    safe direction when the evidence is unclear.
    """
    lock = os.path.join(path, ".lock")
    try:
        with open(lock) as fh:
            pid = int(fh.read().strip())
    except FileNotFoundError:
        return False
    except (OSError, ValueError):
        return True
    return _pid_alive(pid)


def _pid_alive(pid: int) -> bool:
    """Whether ``pid`` names a running process — cross-platform.

    POSIX uses the ``kill(pid, 0)`` idiom. Windows ``os.kill`` rejects signal 0
    with ``OSError [WinError 87]`` regardless of whether the pid exists (a plain
    ``OSError``, so the POSIX handlers below never saw it — this reaped nothing
    and failed the stale-lock test on Windows), so query the process handle
    instead. An unreadable / ambiguous result is treated as ALIVE — refusing to
    delete is the safe direction."""
    if os.name == "nt":  # pragma: no cover - exercised only on Windows CI
        import ctypes

        PROCESS_QUERY_LIMITED_INFORMATION = 0x1000
        STILL_ACTIVE = 259
        kernel32 = ctypes.windll.kernel32
        handle = kernel32.OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, False, pid)
        if not handle:
            return False  # no such process (or access denied → treat as gone)
        try:
            code = ctypes.c_ulong()
            if kernel32.GetExitCodeProcess(handle, ctypes.byref(code)):
                return code.value == STILL_ACTIVE
            return True
        finally:
            kernel32.CloseHandle(handle)
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True  # exists but owned by someone else
    return True


def _dir_size(path: str) -> int:
    total = 0
    for dirpath, _dirnames, filenames in os.walk(path, onerror=lambda _e: None):
        for name in filenames:
            try:
                total += os.lstat(os.path.join(dirpath, name)).st_size
            except OSError:
                continue
    return total


@task(
    name="py-gate",
    help={
        "perf": "Also run the perf-regression gate after the suite (default False).",
        "deselect": (
            "Comma-separated pytest node ids to deselect from the suite. "
            f"Defaults to the known local-only failure ({_LOCAL_DESELECT}); pass "
            '--deselect "" to re-include everything.'
        ),
    },
)
def py_gate(c: Context, perf: bool = True, deselect: str = _LOCAL_DESELECT) -> None:
    """Full pre-commit gate for Python-server changes, in one task.

    Runs, in order: ``ruff check`` + ``ruff format --check`` (via ``lint``); the
    full pytest suite (with the known local-only failure deselected by default);
    and — when ``--perf`` — the perf-regression gate. This is the Python-server
    counterpart to ``rust-gate``; it must be green before committing
    ``src/secantus/**`` work.
    """
    steps = 3 if perf else 2
    # ``==> [k/N] label`` phase markers: drive the Ops Board progress stepper
    # (secantus.opsboard.progress) and give a clear CLI banner per sub-step.
    print(f"==> [1/{steps}] Lint", flush=True)
    lint(c)
    print(f"==> [2/{steps}] Tests", flush=True)
    cmd = "uv run python -m pytest -q"
    for nodeid in (d for d in deselect.split(",") if d.strip()):
        cmd += f" --deselect {shlex.quote(nodeid.strip())}"
    c.run(cmd, pty=True)
    if perf:
        print(f"==> [3/{steps}] Perf", flush=True)
        perf_task(c)


@task(
    name="py-ship",
    help={
        "message": "Commit message (include the Co-Authored-By trailer yourself).",
        "push": "Push HEAD:main after committing (default True; --no-push to stop at commit).",
        "perf": "Run the perf gate as part of py-gate (default True).",
        "deselect": "Passed through to py-gate (see its help).",
    },
)
def py_ship(
    c: Context,
    message: str,
    push: bool = True,
    perf: bool = True,
    deselect: str = _LOCAL_DESELECT,
) -> None:
    """One-shot: run the full Python-server gate, then commit and push HEAD:main.

    Runs ``py-gate`` (aborts the ship if anything is red), stages all tracked
    modifications **except** the vendored submodules and untracked gauge
    artefacts, commits with ``--message``, and — unless ``--no-push`` — pushes the
    current branch to ``main`` (this repo's feature-branch → main flow). New
    (untracked) files must be ``git add``-ed first. Mirrors ``rust-ship``.
    """
    py_gate(c, perf=perf, deselect=deselect)
    c.run(
        "git add -A -- . "
        "':(exclude)vendor' "
        "':(exclude)secantus-data' "
        "':(exclude,glob)docs/validation-report-*-rust-server.md'",
        pty=True,
    )
    c.run(f"git commit -m {shlex.quote(message)}", pty=True)
    if push:
        c.run("git push origin HEAD:main", pty=True)
