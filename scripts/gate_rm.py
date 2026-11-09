#!/usr/bin/env python3
"""The pre-landing gate for a Rust MongoDB server change.

    scripts/gate_rm.py              # from the worktree root
    scripts/gate_rm.py --no-build   # the binaries are already built from HEAD
    scripts/gate_rm.py --only rust-test,lint   # rerun steps a fix only they check
    scripts/gate_rm.py --full       # the storage and PG-server tests even if untouched

The storage crate's own tests (both oplog modes, ~9 min) run only when this
work changes the storage crates (`STORAGE_PATHS`, compared with the branch it
sits on); CI's rust-storage job runs them on every snapshot regardless.

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


#: Paths whose change makes the storage crate's own tests (both oplog modes)
#: worth running. The adapter tests are not in this gate: they drive the
#: command layer against WiredTiger (`command_*_wt.rs`), so most MongoDB
#: items reach them.
STORAGE_PATHS = (
    "crates/secantus-storage/",
    "crates/secantus-wt/",
    "crates/secantus-wiredtiger-sys/",
    "cmake/",
    "vendor/wiredtiger",
)


def gate_base(candidates: tuple[str, ...] = ("origin/rm-batch", "origin/main")) -> str | None:
    """The branch this work sits on: of the candidates that exist, the one
    HEAD is fewest commits ahead of (a fix-one lane branches from rm-batch)."""
    best: tuple[int, str] | None = None
    for ref in candidates:
        r = subprocess.run(
            ["git", "rev-list", "--count", f"{ref}..HEAD"], capture_output=True, text=True
        )
        if r.returncode == 0 and (best is None or int(r.stdout) < best[0]):
            best = (int(r.stdout), ref)
    return best[1] if best else None


def changed_files(base: str) -> list[str]:
    """Every file this work changes: committed since `base`, and uncommitted."""
    out: set[str] = set()
    for cmd in (
        ["git", "diff", "--name-only", f"{base}...HEAD"],
        ["git", "diff", "--name-only", "HEAD"],
    ):
        r = subprocess.run(cmd, capture_output=True, text=True, check=True)
        out.update(line for line in r.stdout.splitlines() if line)
    return sorted(out)


def touches_storage(files: list[str]) -> bool:
    return any(f.startswith(STORAGE_PATHS) for f in files)


def pg_needed(files: list[str]) -> bool:
    """CI's rule (tools/ci_pg_needed.py) for whether the PostgreSQL server's
    answers can have changed: anything but the MongoDB-only paths."""
    path = Path(__file__).resolve().parent.parent / "tools/ci_pg_needed.py"
    spec = importlib.util.spec_from_file_location("ci_pg_needed", path)
    assert spec and spec.loader
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return bool(mod.pg_needed(files))


def is_pg_server_test(path: str) -> bool:
    """A test of the Rust PostgreSQL server, which runs whenever the lane holds
    a `secantusd-pg` and skips when it does not."""
    name = Path(path).name
    return name.startswith("test_rust_pg") or name == "test_pgserver_embedded.py"


class Gate:
    def __init__(self, out: Path, only: set[str] | None = None) -> None:
        self.out = out
        self.only = only
        self.failed = False

    def skip(self, name: str, why: str) -> None:
        """Record a step this run left out on purpose, with why."""
        if self.only is not None and name not in self.only:
            return
        (self.out / f"{name}.rc").write_text("skipped\n")
        (self.out / f"{name}.log").write_text(f"skipped: {why}\n")
        print(f"    {name}=skipped ({why})", flush=True)

    def step(
        self,
        name: str,
        cmd: list[str],
        env: dict[str, str] | None = None,
        cwd: Path | None = None,
    ) -> None:
        if self.only is not None and name not in self.only:
            return
        print(f"==> {name}: {' '.join(cmd)}", flush=True)
        # Each step waits its turn on a busy machine (another lane's gate,
        # another session's build): scripts/wait_for_load.sh has the numbers.
        subprocess.run(["scripts/wait_for_load.sh", f"gate {name}"], check=False)
        start, load = time.monotonic(), os.getloadavg()[0]
        with (self.out / f"{name}.log").open("w") as log:
            rc = subprocess.run(
                cmd,
                stdout=log,
                stderr=subprocess.STDOUT,
                env={**os.environ, **(env or {})},
                cwd=cwd,
            ).returncode
        (self.out / f"{name}.rc").write_text(f"{rc}\n")
        # The load average beside the time: a step's time on this machine is
        # as much the other lane's work as its own (ci-selected ran 100 s or
        # 400-850 s on the same tests, 2026-11-08/09).
        took = int(time.monotonic() - start)
        print(f"    {name}={rc} ({took}s) load {load:.1f}->{os.getloadavg()[0]:.1f}", flush=True)
        if rc != 0:
            self.failed = True


def panicked_logs(out: Path) -> list[str]:
    """The step logs that record a WiredTiger panic. A build step's log is
    not read: a compiler diagnostic quotes WiredTiger's own source, which
    names `WT_PANIC`, whenever a patcher change recompiles it (as
    `gate_pg.sh` leaves out its build log)."""
    return [
        p.name
        for p in sorted(out.glob("*.log"))
        if not p.name.endswith("-build.log")
        and re.search(r"WT_PANIC|Bad address", p.read_text(errors="replace"))
    ]


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--no-build", action="store_true", help="skip the server builds")
    ap.add_argument(
        "--only",
        help="rerun just these comma-separated steps after a fix only they check "
        "(the other steps' results on record are kept, and count)",
    )
    ap.add_argument(
        "--full",
        action="store_true",
        help="run the storage crate's tests and the PostgreSQL server's even when "
        "this work does not touch them",
    )
    args = ap.parse_args()
    only = {s.strip() for s in args.only.split(",") if s.strip()} if args.only else None

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
    if only is None:
        shutil.rmtree(out, ignore_errors=True)
    out.mkdir(parents=True, exist_ok=True)
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

    g = Gate(out, only)
    # The storage crate's own tests (~9 min in both oplog modes) only when this
    # work touches it: most MongoDB items change the command layer alone.
    # CI's rust-storage job still runs them on every snapshot.
    base = gate_base()
    changed = changed_files(base) if base else []
    storage = args.full or base is None or touches_storage(changed)
    # The PostgreSQL server's tests run in ci-selected whenever the lane holds
    # a built `secantusd-pg` (which pg-build below keeps fresh once it exists)
    # and skip otherwise: ci-selected took ~100 s or 400-850 s by that alone
    # (2026-11-08/09). A MongoDB-only change cannot move their answers, and
    # CI's pg-oracle runs them whenever it can.
    if not (args.full or base is None or pg_needed(changed)):
        pg_tests = [f for f in selected if is_pg_server_test(f)]
        selected = [f for f in selected if not is_pg_server_test(f)]
        print(
            f"    not run: {len(pg_tests)} PostgreSQL server test files: "
            f"only MongoDB-only paths changed since {base}",
            flush=True,
        )
    # The cargo lanes: the clean workspace, then each WiredTiger-linked crate
    # the workspace commands do not compile.
    # Every Backlog-ID this work adds is one it reserved, or an item another
    # origin branch already holds (one inherited from rm-batch): a number
    # written by hand meets its twin only on a snapshot PR, a CI cycle
    # later (tools/backlog.py check-ids; #2580 went red on one, 2026-11-09).
    g.step(
        "backlog-ids",
        [str(py), "tools/backlog.py", "check-ids", "--file", "backlog-rust-mongodb.md"],
    )
    g.step("rust-test", ["./inv", "rust-test"])
    g.step("wt-test", ["./inv", "rust-wt-test"])
    if storage:
        g.step("storage-test", ["./inv", "rust-storage-test"])
        # CI's rust-storage job runs the storage crate's tests a second time
        # on the asynchronous oplog (2026-11-07: RM-361's slot timing passed
        # this gate and failed only there).
        g.step(
            "storage-async-oplog",
            ["cargo", "test"],
            cwd=root / "crates/secantus-storage",
            env={**rust_env(root), **ASYNC_OPLOG_ENV},
        )
    else:
        why = f"nothing under {', '.join(STORAGE_PATHS)} changed since {base}"
        g.skip("storage-test", why)
        g.skip("storage-async-oplog", why)
    g.step("adapter-test", ["./inv", "rust-adapter-test"])
    if not args.no_build:
        # What the suites below launch, from HEAD; tools/provenance.py aborts
        # a run against a stale one rather than let it measure old code.
        # The `_secantus_core` extension the suites import is the third
        # artifact tools/provenance.py checks; a commit amended or rebased
        # after the lane's last `./inv sync` leaves it stale and every pytest
        # step aborts before a test runs (2026-11-07, RM-326's re-gate).
        g.step("core-sync", ["./inv", "sync"])
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
    # --durations: which tests a slow run spent its time in, in the step's log.
    durations = ["--durations=25", "--durations-min=5"]
    g.step("ci-selected", [*pytest, *selected, "-n", WORKERS, *timeouts, *durations])
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
    panics = panicked_logs(out)
    if panics:
        print(f"    WT_PANIC / Bad address in: {', '.join(panics)}")
        g.failed = True

    if only is not None:
        # A rerun of some steps: the gate is every step on record.
        print("    every step on record:", flush=True)
        for rc_file in sorted(out.glob("*.rc")):
            rc = rc_file.read_text().strip()
            print(f"      {rc_file.stem}={rc}", flush=True)
            if rc not in ("0", "skipped"):
                g.failed = True
    print(f"gate={int(g.failed)}", flush=True)
    return int(g.failed)


if __name__ == "__main__":
    try:
        sys.exit(main())
    except KeyboardInterrupt:
        print("gate_rm: interrupted", file=sys.stderr)
        sys.exit(130)
