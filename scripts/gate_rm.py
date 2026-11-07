#!/usr/bin/env python3
"""The pre-landing gate for a Rust MongoDB server change.

    scripts/gate_rm.py              # from the worktree root
    scripts/gate_rm.py --no-build   # the binaries are already built from HEAD

What it runs is what CI runs for the Rust MongoDB server, instead of the full
~16,600-test pytest suite (mostly the deprecated Python servers, which CI
stopped testing on 2026-10-20) and ``rust-gate``'s parity step (dropped from
CI the same day):

- the cargo lanes (``rust`` / ``rust-storage``): the clean workspace and
  each WiredTiger-linked crate, fmt + clippy + test;
- the two server builds from HEAD: ``secantusd-rs`` and the embedded
  ``_secantus_server``;
- the ``test`` job's selection (``tools/ci_test_selection.py``), slow tests too;
- the ``storage-engine`` job's named files, read from
  ``.github/workflows/test.yml`` so the two lists cannot drift;
- the backlog's gap and regression tests and ``tests/mongod_live/`` against
  mongod 8.2 (8.2.11 first on PATH when it is installed);
- lint and ruff.

What it does not run, by decision: the other OSes (CI's matrix covers them)
and the PostgreSQL server's suites (``scripts/gate_pg.sh``; add ``--storage``
there when the change touched the shared storage crates).

Every step runs, a failure does not stop the rest, and each step's exit code
goes to ``.scratch/gate/<step>.rc`` with its log beside it. The last line
printed is ``gate=<0|1>``, which is also the exit code. Start it through
``scripts/detached_run.py``: it runs for tens of minutes.
"""

from __future__ import annotations

import argparse
import importlib.util
import os
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path

MONGOD_8211 = Path("/opt/homebrew/opt/mongodb-community@8.2.11/bin")
RUFF = "ruff@0.15.20"
WORKERS = "4"
#: The ``storage-engine`` job's steps whose named test files this gate runs.
STORAGE_ENGINE_STEPS = (
    "Storage + Rust-server smoke tests",
    "Build + smoke the standalone secantusd-rs binary (R7)",
)


#: The environment of CI's "Cargo test (secantus-storage, async-oplog lane)"
#: step in `.github/workflows/test.yml`.
ASYNC_OPLOG_ENV = {"SECANTUS_OPLOG_ASYNC": "1", "SECANTUS_OPLOG_NONLOGGED": "1"}


def rust_env(root: Path) -> dict[str, str]:
    """The WiredTiger / libclang build environment the `./inv rust-*` tasks
    use, for a cargo step this gate runs itself."""
    sys.path.insert(0, str(root))
    from rust_tasks import _rust_env

    return _rust_env()


def not_in_ci(root: Path) -> dict[str, str]:
    """The test files CI deliberately does not run, each with its reason.

    ``tools/ci_test_selection.py`` keeps them: they ``importorskip`` an
    extension the ``test`` job does not build, so CI never runs them. This
    gate's venv has that extension, so without the registry it ran the
    Rust-vs-Python parity suites, which compare against the deprecated
    Python engine and fail by design (2026-11-07: 59 sortkey parity tests,
    entry format 6's descending terminator).
    """
    path = root / "tests/test_ci_runs_rust_server_tests.py"
    spec = importlib.util.spec_from_file_location("ci_registry", path)
    assert spec and spec.loader
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return dict(mod.DELIBERATELY_NOT_IN_CI)


def storage_engine_files(workflow: Path) -> list[str]:
    """The test files the named ``storage-engine`` steps list, in order."""
    text = workflow.read_text()
    files: list[str] = []
    for name in STORAGE_ENGINE_STEPS:
        start = text.find(f"- name: {name}\n")
        if start < 0:
            raise SystemExit(f"gate_rm: step {name!r} not found in {workflow}")
        end = text.find("- name: ", start + 1)
        block = text[start : end if end > 0 else len(text)]
        for f in re.findall(r"tests/[\w/]+\.py", block):
            if f not in files and Path(f).exists():
                files.append(f)
    if not files:
        raise SystemExit("gate_rm: no storage-engine test files found")
    return files


class Gate:
    def __init__(self, out: Path) -> None:
        self.out = out
        self.failed = False

    def step(
        self,
        name: str,
        cmd: list[str],
        env: dict[str, str] | None = None,
        cwd: Path | None = None,
    ) -> None:
        print(f"==> {name}: {' '.join(cmd)}", flush=True)
        # Each step waits its turn on a busy machine (another lane's gate,
        # another session's build): scripts/wait_for_load.sh has the numbers.
        subprocess.run(["scripts/wait_for_load.sh", f"gate {name}"], check=False)
        start = time.monotonic()
        with (self.out / f"{name}.log").open("w") as log:
            rc = subprocess.run(
                cmd,
                stdout=log,
                stderr=subprocess.STDOUT,
                env={**os.environ, **(env or {})},
                cwd=cwd,
            ).returncode
        (self.out / f"{name}.rc").write_text(f"{rc}\n")
        print(f"    {name}={rc} ({int(time.monotonic() - start)}s)", flush=True)
        if rc != 0:
            self.failed = True


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--no-build", action="store_true", help="skip the two server builds")
    args = ap.parse_args()

    root = Path(
        subprocess.run(
            ["git", "rev-parse", "--show-toplevel"], capture_output=True, text=True, check=True
        ).stdout.strip()
    )
    os.chdir(root)
    py = root / ".venv/bin/python"
    if not py.exists():
        print(f"no worktree venv at {py}: run ./inv sync first", file=sys.stderr)
        return 2
    out = root / ".scratch/gate"
    shutil.rmtree(out, ignore_errors=True)
    out.mkdir(parents=True)
    os.environ.setdefault("CARGO_BUILD_JOBS", "3")
    if MONGOD_8211.is_dir():
        os.environ["PATH"] = f"{MONGOD_8211}{os.pathsep}{os.environ['PATH']}"
    se_files = storage_engine_files(root / ".github/workflows/test.yml")
    excluded = not_in_ci(root)
    selected = [
        f
        for f in subprocess.run(
            [str(py), "tools/ci_test_selection.py"], capture_output=True, text=True, check=True
        ).stdout.split()
        if Path(f).name not in excluded
    ]
    for name, why in sorted(excluded.items()):
        print(f"    not run, as in CI: {name}: {why}", flush=True)

    g = Gate(out)
    # The cargo lanes: the clean workspace, then each WiredTiger-linked crate
    # the workspace commands do not compile.
    g.step("rust-test", ["./inv", "rust-test"])
    g.step("wt-test", ["./inv", "rust-wt-test"])
    g.step("storage-test", ["./inv", "rust-storage-test"])
    # CI's rust-storage job runs the storage crate's tests a second time on
    # the asynchronous oplog (2026-11-07: RM-361's slot timing passed this
    # gate and failed only there).
    g.step(
        "storage-async-oplog",
        ["cargo", "test"],
        cwd=root / "crates/secantus-storage",
        env={**rust_env(root), **ASYNC_OPLOG_ENV},
    )
    g.step("adapter-test", ["./inv", "rust-adapter-test"])
    if not args.no_build:
        # What the suites below launch, from HEAD; tools/provenance.py aborts
        # a run against a stale one rather than let it measure old code.
        g.step("binary-build", ["./inv", "rust-binary-build"])
        g.step("server-build", ["./inv", "rust-server-build"])
        # tools/provenance.py refuses a run while ANY artifact it knows is
        # stale, not only the ones this gate launches. A lane that once ran
        # `gate_pg.sh --storage` holds a `secantusd-pg`, so rebuild it too
        # (2026-11-07: RM-361's gate aborted every pytest step on one built
        # before a rebase).
        if (root / "crates/secantus-pg/target/debug/secantusd-pg").exists():
            g.step("pg-build", ["./inv", "rust-pgserver-build"])

    pytest = [str(py), "-m", "pytest", "-p", "no:cacheprovider", "-q", "-rs"]
    timeouts = ["--timeout=120", "--timeout-method=thread", "--max-worker-restart=3"]
    g.step("ci-selected", [*pytest, *selected, "-n", WORKERS, *timeouts])
    g.step("ci-selected-slow", [*pytest, "-m", "slow", *selected, "-n", WORKERS, *timeouts])
    g.step("storage-engine", [*pytest, *se_files, "-o", "addopts=", "-n", WORKERS])
    g.step(
        "concurrency",
        [*pytest, "tests/test_mongo_server_concurrency.py", "-k", "rust", "-o", "addopts="],
    )
    g.step(
        "backlog",
        [
            str(py),
            "tools/backlog.py",
            "verify",
            "--file",
            "backlog-rust-mongodb.md",
            "--regressions",
        ],
        env={"SECANTUS_BACKLOG_WORKERS": WORKERS},
    )
    g.step(
        "mongod-live",
        [*pytest, "tests/mongod_live", "-m", "mongod_live", "-o", "addopts=", "-n", WORKERS],
    )
    g.step("lint", ["./inv", "lint"])
    g.step("ruff-check", ["uvx", RUFF, "check", "."])
    g.step("ruff-format", ["uvx", RUFF, "format", "--check", "."])

    # A WiredTiger panic in any log is the headline, whatever the exit codes say.
    panics = [
        p.name
        for p in sorted(out.glob("*.log"))
        if re.search(r"WT_PANIC|Bad address", p.read_text(errors="replace"))
    ]
    if panics:
        print(f"    WT_PANIC / Bad address in: {', '.join(panics)}")
        g.failed = True

    print(f"gate={int(g.failed)}", flush=True)
    return int(g.failed)


if __name__ == "__main__":
    try:
        sys.exit(main())
    except KeyboardInterrupt:
        print("gate_rm: interrupted", file=sys.stderr)
        sys.exit(130)
