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

import shlex

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
    lint(c)
    cmd = "uv run python -m pytest -q"
    for nodeid in (d for d in deselect.split(",") if d.strip()):
        cmd += f" --deselect {shlex.quote(nodeid.strip())}"
    c.run(cmd, pty=True)
    if perf:
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
