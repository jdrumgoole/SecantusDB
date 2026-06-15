from __future__ import annotations

import json
import os
import pathlib
import re
import shlex
import subprocess
import time
import urllib.error
import urllib.request

from invoke.context import Context
from invoke.tasks import task


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
    c.run(f"uv run python -m pytest -p no:xdist {shlex.quote(nodeid)}", pty=True)


@task
def perf(c: Context) -> None:
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
    c.run("uv run ruff check src tests", pty=True)
    c.run("uv run ruff format --check src tests", pty=True)


@task
def fmt(c: Context) -> None:
    c.run("uv run ruff format src tests", pty=True)
    c.run("uv run ruff check --fix src tests", pty=True)


# --- Rust core (Phase 1 of the Python -> Rust rewrite) --------------------
# The Rust side is a Cargo workspace under crates/:
#   * crates/secantus-core    — the pure-Rust operator engines (no PyO3).
#   * crates/secantus-core-py — the PyO3 bindings that wrap it and build the abi3
#                               extension `_secantus_core` (the `secantus-core`
#                               wheel) via maturin.
# It is additive: the engine shims delegate to it only when the matching
# component is enabled. See tasks/rust-rewrite-plan.md and
# tasks/rust-rewrite-spike-findings.md.

_RUST_WORKSPACE_DIR = "crates"
_RUST_BINDINGS_DIR = "crates/secantus-core-py"


@task(name="rust-test")
def rust_test(c: Context) -> None:
    """cargo fmt --check, clippy (warnings-as-errors), unit tests (whole workspace)."""
    c.run(f"cd {_RUST_WORKSPACE_DIR} && cargo fmt --check", pty=True)
    c.run(f"cd {_RUST_WORKSPACE_DIR} && cargo clippy --all-targets -- -D warnings", pty=True)
    c.run(f"cd {_RUST_WORKSPACE_DIR} && cargo test", pty=True)


@task(name="rust-build")
def rust_build(c: Context) -> None:
    """Build the abi3 wheel for the Rust core into target/wheels/."""
    c.run(
        f"cd {_RUST_BINDINGS_DIR} && uv tool run maturin build --release",
        pty=True,
        env={"VIRTUAL_ENV": ""},
    )


@task(name="rust-parity")
def rust_parity(c: Context) -> None:
    """Build the Rust core and run the leaf-engine parity suites against it.

    Builds the extension, then runs the parity tests (sortkey + query) in an
    isolated interpreter (pymongo + the freshly built wheel) so they do not
    require the WiredTiger C extension to be installed. This mirrors how the
    parity gate runs in a WiredTiger-less environment; full CI also runs them
    via the normal pytest suite once the project wheel is built.

    ``--reinstall-package`` busts uv's cache: the wheel keeps the same
    name/version across rebuilds, so without it a stale build would be reused.
    """
    import glob

    c.run(
        f"cd {_RUST_BINDINGS_DIR} && uv tool run maturin build --release --out dist",
        pty=True,
    )
    wheels = sorted(glob.glob(f"{_RUST_BINDINGS_DIR}/dist/*.whl"))
    if not wheels:
        raise SystemExit("no wheel produced by maturin")
    c.run(
        "uv run --no-project --reinstall-package secantus-core "
        f"--with pymongo --with pytest --with {shlex.quote(wheels[-1])} "
        "python -m pytest tests/test_rust_sortkey_parity.py tests/test_rust_query_parity.py "
        "tests/test_rust_update_parity.py tests/test_rust_expressions_parity.py "
        "tests/test_rust_projection_parity.py tests/test_rust_diff_parity.py "
        "tests/test_rust_aggregate_parity.py "
        "-o addopts= -p no:cacheprovider -q",
        pty=True,
    )


# The WiredTiger FFI / storage foundation (Phase 4) is a standalone crate outside
# the secantus-core workspace because it links the vendored WiredTiger C library.
_RUST_WT_DIR = "crates/secantus-wt"


@task(name="rust-wt-test")
def rust_wt_test(c: Context) -> None:
    """fmt/clippy/test the secantus-wt WiredTiger FFI crate.

    Needs WiredTiger present: either set SECANTUS_WT_INCLUDE / SECANTUS_WT_LIB,
    or have it under build/*/wt-build (the project CMake output) or /tmp/wt-build.
    bindgen needs libclang (set LIBCLANG_PATH if not auto-found).
    """
    c.run(f"cd {_RUST_WT_DIR} && cargo fmt --check", pty=True)
    c.run(f"cd {_RUST_WT_DIR} && cargo clippy --all-targets -- -D warnings", pty=True)
    c.run(f"cd {_RUST_WT_DIR} && cargo test", pty=True)


# The Rust storage layer (Phase 4 sub-phase 1+), built on secantus-wt +
# secantus-core. Also standalone (links WiredTiger transitively via secantus-wt).
_RUST_STORAGE_DIR = "crates/secantus-storage"


@task(name="rust-storage-test")
def rust_storage_test(c: Context) -> None:
    """fmt/clippy/test the secantus-storage crate (the Rust Storage layer).

    Same WiredTiger / libclang prerequisites as ``rust-wt-test`` (it links
    WiredTiger transitively through secantus-wt).
    """
    c.run(f"cd {_RUST_STORAGE_DIR} && cargo fmt --check", pty=True)
    c.run(f"cd {_RUST_STORAGE_DIR} && cargo clippy --all-targets -- -D warnings", pty=True)
    c.run(f"cd {_RUST_STORAGE_DIR} && cargo test", pty=True)


# The PyO3 bindings that expose the Rust storage layer to Python as the
# WiredTiger-linking _secantus_storage extension.
_RUST_STORAGE_PY_DIR = "crates/secantus-storage-py"


@task(name="rust-storage-py")
def rust_storage_py(c: Context) -> None:
    """Build the _secantus_storage extension and run its Python smoke test.

    Builds the WiredTiger-linking wheel with maturin, then runs the smoke test in
    an isolated interpreter (pymongo + the freshly built wheel) so it doesn't need
    the project's own WiredTiger extension installed. Same WiredTiger / libclang
    prerequisites as ``rust-wt-test``.
    """
    import glob

    c.run(
        f"cd {_RUST_STORAGE_PY_DIR} && uv tool run maturin build --release --out dist",
        pty=True,
    )
    wheels = sorted(glob.glob(f"{_RUST_STORAGE_PY_DIR}/dist/*.whl"))
    if not wheels:
        raise SystemExit("no wheel produced by maturin")
    c.run(
        "uv run --no-project --reinstall-package secantus-storage "
        f"--with pymongo --with pytest --with {shlex.quote(wheels[-1])} "
        "python -m pytest tests/test_rust_storage_smoke.py "
        "-o addopts= -p no:cacheprovider -q",
        pty=True,
    )


# The standalone Rust server binary (R7), over the same crates the embedded
# _secantus_server handle uses. Links WiredTiger, so it lives outside the clean
# workspace like secantus-storage-adapter.
_RUST_BINARY_DIR = "crates/secantusdb"


@task(name="rust-binary-test")
def rust_binary_test(c: Context) -> None:
    """Build the standalone ``secantusdb`` binary and run its smoke test.

    Builds the WiredTiger-linking bin crate, then launches it from
    tests/test_rust_binary_smoke.py (ephemeral port, pymongo round-trip,
    clean SIGTERM exit) in an isolated interpreter. Same WiredTiger /
    libclang prerequisites as ``rust-wt-test``.
    """
    c.run(f"cd {_RUST_BINARY_DIR} && cargo build", pty=True)
    c.run(
        "uv run --no-project --with pymongo --with pytest "
        "python -m pytest tests/test_rust_binary_smoke.py "
        "-o addopts= -p no:cacheprovider -q",
        pty=True,
    )


@task
def serve(c: Context, host: str = "127.0.0.1", port: int = 27017) -> None:
    c.run(
        f"uv run python -m secantus --host {shlex.quote(host)} --port {int(port)}",
        pty=True,
    )


@task(
    help={
        "uri": "MongoDB URI of the target server (default: mongodb://127.0.0.1:27017/).",
        "db": "Target database (default: harness).",
        "collection": "Target collection (default: inserts_8k).",
        "count": "Number of documents to insert. Omit for continuous mode.",
        "drop": "Drop the target collection before inserting.",
        "progress-every": "Print a progress line every N inserts (default: 1000; 0 to disable).",
    }
)
def load(
    c: Context,
    uri: str = "mongodb://127.0.0.1:27017/",
    db: str = "harness",
    collection: str = "inserts_8k",
    count: int = 0,
    drop: bool = False,
    progress_every: int = 1000,
) -> None:
    """Insert standard 8 KiB documents with a sequence counter.

    Pairs with ``invoke serve``: bring up a server in one terminal and
    point this at it in another. ``--count 0`` (the default) means run
    continuously until Ctrl-C; pass a positive integer for a bounded run.
    Each document carries a monotonic ``n`` field (1, 2, 3, ...) and an
    8192-byte payload, so total BSON size is comfortably ≥ 8 KiB.
    """
    # ``--no-sync`` skips uv's project-rebuild check: invoking the harness
    # shouldn't trigger a multi-minute CMake/WiredTiger rebuild every
    # time. Same pattern the docs / release tasks use.
    cmd = (
        "uv run --no-sync python -m bench.load_writer"
        f" --uri {shlex.quote(uri)}"
        f" --db {shlex.quote(db)}"
        f" --collection {shlex.quote(collection)}"
        f" --progress-every {int(progress_every)}"
    )
    if count > 0:
        cmd += f" --count {int(count)}"
    if drop:
        cmd += " --drop"
    c.run(cmd, pty=True)


@task(
    help={
        "duration": "Total run time in seconds (default: 180).",
        "min-interval": "Minimum seconds between SIGKILLs (default: 5).",
        "max-interval": "Maximum seconds between SIGKILLs (default: 15).",
        "port": "Server port (default: auto-pick a free port).",
        "storage-path": "WiredTiger storage dir (default: tempdir, removed at end).",
        "no-load": "Don't auto-start the load_writer (chaos only).",
        "seed": "RNG seed for kill timing (default: random).",
        "batch-size": "Documents per insert call in the writer (default: 1).",
    }
)
def chaos(
    c: Context,
    duration: float = 180.0,
    min_interval: float = 5.0,
    max_interval: float = 15.0,
    port: int = 0,
    storage_path: str = "",
    no_load: bool = False,
    seed: int = 0,
    batch_size: int = 1,
) -> None:
    """Chaos monkey: random SIGKILL/restart of SecantusDB under live load.

    Spawns SecantusDB on a free port with on-disk WiredTiger storage,
    optionally starts ``bench.load_writer`` against it, then kills and
    restarts the server at random intervals. After ``--duration``
    seconds prints a report: kills, downtime, persisted docs, gaps in
    the writer's ``n`` sequence (gaps == inserts that fell during
    outages or were not durably committed before the kill).
    """
    cmd = (
        "uv run --no-sync python -m bench.chaos"
        f" --duration {float(duration)}"
        f" --min-interval {float(min_interval)}"
        f" --max-interval {float(max_interval)}"
    )
    if port:
        cmd += f" --port {int(port)}"
    if storage_path:
        cmd += f" --storage-path {shlex.quote(storage_path)}"
    if no_load:
        cmd += " --no-load"
    if seed:
        cmd += f" --seed {int(seed)}"
    if batch_size > 1:
        cmd += f" --batch-size {int(batch_size)}"
    c.run(cmd, pty=True)


@task(
    help={
        "duration": "Wall-clock seconds per writer count (default: 30).",
        "batch-size": "Documents per insert call (default: 100).",
        "writers": 'Comma-separated writer counts (default: "1,2,4").',
        "shared-collection": "All writers share one collection (max contention).",
    }
)
def concurrency(
    c: Context,
    duration: float = 30.0,
    batch_size: int = 100,
    writers: str = "1,2,4",
    shared_collection: bool = False,
) -> None:
    """N-writer scaling benchmark for the storage layer.

    Phase 0 instrument from ``tasks/wt-concurrency-plan.md``. Spawns
    one server, then runs each writer count back to back; prints
    aggregate throughput + a scaling ratio per row. Today's expected
    number is 0.35x at N=2 on a single collection (Storage._lock
    contention dominates); Phase 2 has to push it above 1.5x.
    """
    cmd = (
        "uv run --no-sync python -m bench.concurrency"
        f" --duration {float(duration)}"
        f" --batch-size {int(batch_size)}"
        f" --writers {shlex.quote(writers)}"
    )
    if shared_collection:
        cmd += " --shared-collection"
    c.run(cmd, pty=True)


@task(
    name="rw-harness",
    help={
        "workers": "Number of independent reader/writer processes (default: 4).",
        "count": "Documents each worker writes then stops (default: 1000).",
        "duration": "Run each worker N seconds instead of a fixed count (overrides --count).",
        "server": "Server hosting: daemon (default) | embedded | external.",
        "uri": "Server URI when --server external (default: mongodb://127.0.0.1:27018/).",
        "payload-bytes": "Random payload size per document (default: 256).",
        "sync-on-commit": "Start the server with --sync-on-commit (fsync every commit).",
    },
)
def rw_harness(
    c: Context,
    workers: int = 4,
    count: int = 1000,
    duration: float = 0.0,
    server: str = "daemon",
    uri: str = "mongodb://127.0.0.1:27018/",
    payload_bytes: int = 256,
    sync_on_commit: bool = False,
) -> None:
    """Concurrent read/write validation harness.

    Spawns ``--workers`` independent processes that simultaneously read
    and write a shared collection with the highest write/read safety
    (w:majority, j:true, readConcern:majority, retryWrites/Reads). Every
    read is checksum-validated in flight; a final paginated sweep
    re-verifies every document and reconciles per-worker counts. The
    server can be hosted as a daemon subprocess (default), embedded
    in-process, or pointed at an external URI for differential testing.
    """
    cmd = (
        "uv run --no-sync python -m bench.rw_harness"
        f" --workers {int(workers)}"
        f" --server {shlex.quote(server)}"
        f" --payload-bytes {int(payload_bytes)}"
    )
    if duration > 0:
        cmd += f" --duration {float(duration)}"
    else:
        cmd += f" --count {int(count)}"
    if server == "external":
        cmd += f" --uri {shlex.quote(uri)}"
    if sync_on_commit:
        cmd += " --sync-on-commit"
    c.run(cmd, pty=True)


@task(
    help={
        "uri": "MongoDB URI to administer.",
        "port": "Local HTTP port (0 = pick a free one).",
        "no_window": "Run headless (no pywebview window). Useful for CI.",
        "token": "Override the auth token. Default: ~/.secantus/admin-token.",
    }
)
def admin(
    c: Context,
    uri: str = "mongodb://127.0.0.1:27017",
    port: int = 0,
    no_window: bool = False,
    token: str = "",
) -> None:
    """Launch the SecantusDB admin web UI.

    Uses ``--extra admin`` so uv pulls in fastapi / uvicorn / pywebview
    on first run; the base wheel deliberately doesn't ship them so an
    embedded ``SecantusDBServer`` user isn't paying for the GUI stack.
    """
    cmd = [
        "uv",
        "run",
        "--extra",
        "admin",
        "secantusdb-admin",
        "--uri",
        uri,
        "--port",
        str(port),
    ]
    if no_window:
        cmd.append("--no-window")
    if token:
        cmd.extend(["--token", token])
    c.run(" ".join(cmd), pty=True)


@task
def docs(c: Context, builder: str = "html", clean: bool = False) -> None:
    # --no-sync skips uv's project rebuild check: docs only need the Python
    # source for autodoc, never a fresh WiredTiger C-extension build. Falling
    # through to `uv sync` here would invoke scikit-build-core's isolated
    # build env, which is sensitive to host cmake/swig setup and unnecessary
    # for a docs build.
    if clean:
        c.run("rm -rf docs/_build", pty=True)
    qb = shlex.quote(builder)
    c.run(
        f"uv run --no-sync sphinx-build -W --keep-going -b {qb} docs docs/_build/{qb}",
        pty=True,
    )


@task(name="docs-serve")
def docs_serve(c: Context, port: int = 8000) -> None:
    docs(c)
    c.run(
        f"uv run --no-sync python -m http.server {port} --directory docs/_build/html",
        pty=True,
    )


@task(
    help={
        "server": (
            "Which SecantusDB server the gauge runs against: 'python' (the "
            "pure-Python SecantusDBServer; the headline gauge, default) or "
            "'rust' (the Rust server via the _secantus_server embedded "
            "handle; the R8 conformance gate)."
        ),
    }
)
def validate(c: Context, server: str = "python") -> None:
    """Run pymongo's vendored test suite against an embedded SecantusDB.

    Generates docs/validation-report.md with a per-category pass / fail /
    skip / pass-rate breakdown — the "MongoDB compatibility" gauge.

    ``--server rust`` runs the same unmodified suite against the Rust
    server instead and writes docs/validation-report-rust-server.md (the
    R8 gate from tasks/rust-server-plan.md). It needs the WT-linking
    ``_secantus_server`` extension importable in the project venv — build
    it into the editable install with::

        SKBUILD_CMAKE_DEFINE=SECANTUS_BUILD_STORAGE_ENGINE=ON \\
            uv sync --extra dev --reinstall-package SecantusDB
    """
    import pathlib

    from pymongo_validation.include_paths import DESELECT_TESTS, INCLUDE

    if server not in ("python", "rust"):
        raise SystemExit(f"--server must be 'python' or 'rust', got {server!r}")

    if not pathlib.Path("vendor/pymongo-tests/test").exists():
        c.run("git submodule update --init --recursive", pty=True)

    pathlib.Path(".validation").mkdir(exist_ok=True)
    paths = " ".join(INCLUDE)
    deselect = " ".join(f"--deselect={t}" for t in DESELECT_TESTS)
    suffix = "" if server == "python" else "-rust-server"
    raw_json = f".validation/raw{suffix}.json"
    report = f"docs/validation-report{suffix}.md"
    # `-p no:cacheprovider`: don't pollute pymongo's tree with .pytest_cache.
    # `-n1 -o addopts=`: pymongo's tests aren't parallel-safe (shared DBs), so
    #   exactly ONE xdist worker — serial semantics, but a pytest-timeout
    #   process kill on a hung test only takes out the worker (xdist records
    #   the crash, restarts the worker, and the json report survives). A bare
    #   no-xdist run would lose the whole report to the first hang.
    #   `--max-worker-restart=200`: don't let repeated hangs end the run.
    # `-o timeout=120`: tighter than the project-wide 600s — a gauge test
    #   that blocks >2 min against SecantusDB is a conformance failure worth
    #   recording, and at 600s a handful of hangs would add hours.
    # `-p pymongo_validation.plugin`: load our embedded-server bootstrap (the
    #   CONTROLLER starts the server pre-conftest; workers inherit the env).
    # `--continue-on-collection-errors`: a collection failure in one file
    #   shouldn't abort the whole run — we want every category measured.
    # `-c pyproject.toml` forces pytest to use OUR config; without it pytest
    # picks up vendor/pymongo-tests/pyproject.toml (closer to the test files)
    # which has options for plugins we don't load (pytest-asyncio etc).
    # `-o addopts= -o testpaths=`: clear the project-wide xdist + tests/ scoping
    # from our pyproject; this run uses positional paths.
    # PYTHONPATH=. so pytest can import our `pymongo_validation` plugin.
    c.run(
        f"SECANTUS_GAUGE_SERVER={server} "
        "PYTHONPATH=. uv run --no-sync python -m pytest "
        "-c pyproject.toml "
        "-o addopts= -o testpaths= -o timeout=120 "
        "-n1 --max-worker-restart=200 "
        "-p no:cacheprovider -p pymongo_validation.plugin "
        "--continue-on-collection-errors "
        f"--json-report --json-report-file={raw_json} "
        f"--no-header --tb=no -q {deselect} {paths}",
        pty=True,
        warn=True,
    )
    c.run(
        "uv run --no-sync python -m pymongo_validation.generate_report "
        f"--server {server} {raw_json} {report}",
        pty=True,
    )
    print(f"\nWrote {report}")


@task(name="validate-go")
def validate_go(c: Context) -> None:
    """Run mongo-go-driver's tests against an embedded SecantusDB.

    Generates docs/validation-report-go.md with a per-package pass /
    fail / skip / pass-rate breakdown — the Go-driver analogue of the
    pymongo gauge. Requires `go` on PATH (1.21+).
    """
    import pathlib

    # Need both the outer submodule AND its nested `testdata/specifications`
    # submodule (driver-spec test data — without it the bson-corpus tests
    # fail on missing JSON files).
    if (
        not pathlib.Path("vendor/mongo-go-driver/go.mod").exists()
        or not pathlib.Path("vendor/mongo-go-driver/testdata/specifications/source").is_dir()
    ):
        c.run("git submodule update --init --recursive", pty=True)

    pathlib.Path(".validation").mkdir(exist_ok=True)
    c.run(
        "PYTHONPATH=. uv run --no-sync python -m go_validation.runner",
        pty=True,
        warn=True,  # report is the deliverable
    )
    c.run(
        "uv run --no-sync python -m go_validation.generate_report "
        ".validation/go-raw.ndjson docs/validation-report-go.md",
        pty=True,
    )
    print("\nWrote docs/validation-report-go.md")


@task(name="validate-node")
def validate_node(c: Context) -> None:
    """Run mongo-node-driver's tests against an embedded SecantusDB.

    Generates docs/validation-report-node.md with a per-category pass /
    fail / pending / pass-rate breakdown — the Node-driver analogue of
    the pymongo and Go-driver gauges. Requires Node.js (>=20) and npm
    on PATH. First run does a one-time `npm install` (~1-2 min) inside
    vendor/node-mongodb-native/.
    """
    import pathlib

    if not pathlib.Path("vendor/node-mongodb-native/package.json").exists():
        c.run("git submodule update --init --recursive", pty=True)

    pathlib.Path(".validation").mkdir(exist_ok=True)
    c.run(
        "PYTHONPATH=. uv run --no-sync python -m node_validation.runner",
        pty=True,
        warn=True,
    )
    c.run(
        "uv run --no-sync python -m node_validation.generate_report "
        ".validation/node-raw.json docs/validation-report-node.md",
        pty=True,
    )
    print("\nWrote docs/validation-report-node.md")


@task(name="validate-ruby")
def validate_ruby(c: Context) -> None:
    """Run mongo-ruby-driver's tests against an embedded SecantusDB.

    Generates docs/validation-report-ruby.md with a per-category pass /
    fail / pending / pass-rate breakdown — the Ruby-driver analogue of
    the pymongo / Go / Node / Java gauges. Requires Ruby (>= 2.7) and
    bundler on PATH (e.g. `brew install ruby` on macOS, then add
    `/opt/homebrew/opt/ruby/bin` to PATH). First run does a one-time
    `bundle install` (~1-2 min) inside vendor/mongo-ruby-driver/.
    """
    import pathlib

    if not pathlib.Path("vendor/mongo-ruby-driver/mongo.gemspec").exists():
        c.run("git submodule update --init --recursive", pty=True)

    pathlib.Path(".validation").mkdir(exist_ok=True)
    c.run(
        "PYTHONPATH=. uv run --no-sync python -m ruby_validation.runner",
        pty=True,
        warn=True,
    )
    c.run(
        "uv run --no-sync python -m ruby_validation.generate_report "
        ".validation/ruby-raw.json docs/validation-report-ruby.md",
        pty=True,
    )
    print("\nWrote docs/validation-report-ruby.md")


@task(name="validate-java")
def validate_java(c: Context) -> None:
    """Run mongo-java-driver's tests against an embedded SecantusDB.

    Generates docs/validation-report-java.md with a per-module pass /
    fail / skipped / pass-rate breakdown — the Java-driver analogue of
    the pymongo / Go / Node gauges. Requires a JDK (>=8) on PATH; uses
    the gradle wrapper the driver ships, so no system Gradle install
    needed. First run downloads the gradle distribution + dependencies
    (~150 MB) into ~/.gradle/.
    """
    import pathlib

    # The driver pulls in MongoDB driver-spec test data via a nested
    # submodule (testing/resources/specifications) — without it the
    # bson corpus / vector tests fail with `initializationError` on
    # missing JSON files. Same pattern as the go-driver gauge.
    if (
        not pathlib.Path("vendor/mongo-java-driver/gradlew").exists()
        or not pathlib.Path(
            "vendor/mongo-java-driver/testing/resources/specifications/source"
        ).is_dir()
    ):
        c.run("git submodule update --init --recursive", pty=True)

    pathlib.Path(".validation").mkdir(exist_ok=True)
    c.run(
        "PYTHONPATH=. uv run --no-sync python -m java_validation.runner",
        pty=True,
        warn=True,
    )
    c.run(
        "uv run --no-sync python -m java_validation.generate_report "
        ".validation/java-results docs/validation-report-java.md",
        pty=True,
    )
    print("\nWrote docs/validation-report-java.md")


@task(name="validate-rust")
def validate_rust(c: Context) -> None:
    """Run mongo-rust-driver's tests against an embedded SecantusDB.

    Generates docs/validation-report-rust.md with a per-module pass /
    fail / ignored / pass-rate breakdown — the Rust-driver analogue of
    the pymongo / Go / Node / Java / Ruby gauges. Requires Rust
    (>= 1.88) on PATH (``brew install rust`` on macOS; ``rustup`` on
    linux). First run does a one-time cargo build (~1-2 min) inside
    vendor/mongo-rust-driver/; subsequent runs reuse ``target/`` and
    complete in seconds for the curated include set.
    """
    import pathlib

    if not pathlib.Path("vendor/mongo-rust-driver/Cargo.toml").exists():
        c.run("git submodule update --init --recursive", pty=True)

    pathlib.Path(".validation").mkdir(exist_ok=True)
    c.run(
        "PYTHONPATH=. uv run --no-sync python -m rust_validation.runner",
        pty=True,
        warn=True,
    )
    c.run(
        "uv run --no-sync python -m rust_validation.generate_report "
        ".validation/rust-raw.json docs/validation-report-rust.md",
        pty=True,
    )
    print("\nWrote docs/validation-report-rust.md")


@task(name="validate-all")
def validate_all(c: Context) -> None:
    """Run all six driver gauges in parallel.

    Local equivalent of the CI ``.github/workflows/validate.yml`` matrix:
    fans out ``invoke validate / validate-go / validate-node /
    validate-java / validate-ruby / validate-rust`` across a 6-wide
    thread pool. Each gauge spawns its own SecantusDB daemon on a
    kernel-assigned ephemeral port + its own tempdir, so they don't
    collide.

    Wall-clock is the slowest single gauge (usually node or java); on
    a dev laptop ~5x faster than the previous serial run. Output from
    the five subprocesses interleaves on stdout/stderr — accepted
    trade-off for live progress. Exit code is non-zero if any gauge
    failed.
    """
    import concurrent.futures
    import subprocess
    import sys

    GAUGES = [
        ("pymongo", "validate"),
        ("go", "validate-go"),
        ("node", "validate-node"),
        ("java", "validate-java"),
        ("ruby", "validate-ruby"),
        ("rust", "validate-rust"),
    ]

    def _run(name_task: tuple[str, str]) -> tuple[str, int]:
        name, task_name = name_task
        # Stream stdout/stderr directly so the user gets live progress.
        # We don't capture — interleaving is the price of parallelism.
        result = subprocess.run(
            ["uv", "run", "--no-sync", "python", "-m", "invoke", task_name],
            check=False,
        )
        return name, result.returncode

    # Run gauges serially. Earlier attempts at 5-way and 3-way
    # parallelism produced flaky failures specifically driven by
    # OS-scheduler timing: Python's GIL pins the daemon's accept loop
    # to one bytecode runner per process, so when N daemons + N
    # driver test processes contend for CPU, individual ``hello``
    # handshakes occasionally exceed the driver's
    # ``serverSelectionTimeoutMS`` (30 s default). The Go gauge is
    # the most sensitive (the gauge's
    # ``TestIndexView/{drop_one,drop_all,create_many/*}`` subtests
    # observe ``Type: Unknown`` topology and fail with
    # ``context deadline exceeded``); the Java gauge's
    # ``ContextProviderTest#contextShouldBeAvailableInCommandEvents``
    # also flaked. Both pass cleanly when each gauge has the daemon's
    # CPU to itself. Wall-clock cost: ~12 min serial vs ~7 min
    # parallel — small enough to be worth the determinism.
    max_workers = 1
    print(
        f"validate-all: dispatching {len(GAUGES)} gauges serially\n",
        flush=True,
    )
    with concurrent.futures.ThreadPoolExecutor(max_workers=max_workers) as pool:
        futures = {pool.submit(_run, ng): ng[0] for ng in GAUGES}
        results = {
            future.result()[0]: future.result()[1]
            for future in concurrent.futures.as_completed(futures)
        }

    print("\n=== validate-all summary ===", flush=True)
    failed = []
    for name, _ in GAUGES:
        rc = results.get(name, 1)
        status = "ok" if rc == 0 else f"FAILED (rc={rc})"
        print(f"  {name:<8} {status}", flush=True)
        if rc != 0:
            failed.append(name)
    if failed:
        print(f"\n{len(failed)} gauge(s) failed: {', '.join(failed)}", flush=True)
        sys.exit(1)


@task(name="validate-summary")
def validate_summary(c: Context) -> None:
    """Generate ``docs/validation-summary.md`` from the five gauges' raw output.

    Each gauge writes its raw artifact to ``.validation/`` (``raw.json``,
    ``go-raw.ndjson``, ``node-raw.json``, ``ruby-raw.json``,
    ``java-results/``). This task reads them all and renders one table
    in ``docs/validation-summary.md`` so the five gauges can be compared
    like for like — every row counts one assertion outcome.

    Gauges that have never been run (no raw artifact) are silently
    omitted from the table. Run ``invoke validate-all`` first if you
    want a complete snapshot.
    """
    c.run("uv run --no-sync python -m validation_summary.generate", pty=True)


@task(name="validate-readme")
def validate_readme(c: Context) -> None:
    """HEAD-check every URL in the published PyPI README.

    PyPI doesn't know our git repo, so any relative URL in `README.md`
    renders as a broken link on the project page. This task fetches
    the description PyPI is actually serving, extracts every link/img
    URL, and reports each one's reachability — a thin wrapper over
    `pytest -m online tests/test_pypi_readme_links.py` so failures
    are easy to read in a terminal.

    Run it after every release. Network-dependent and depends on the
    package being published, so it's deliberately excluded from
    `invoke test` (the `online` marker filters it out by default).
    """
    c.run(
        "uv run --no-sync python -m pytest "
        "-p no:xdist -o addopts= -m online -v "
        "tests/test_pypi_readme_links.py",
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


_VERSION_RE = re.compile(r"^(\d+)\.(\d+)\.(\d+)([ab]\d+|rc\d+)?$")


@task
def release(c: Context, version: str) -> None:
    """Cut a release: prepare + finalize, end-to-end.

    The canonical one-shot release workflow (see `## Releases` in
    CLAUDE.md). Internally calls ``release-prepare`` (fast,
    foreground-friendly) followed by ``release-finalize`` (long
    polling). When invoked from a sub-agent, prefer running the two
    phases separately so the polling phase can use
    ``run_in_background=true`` and escape the per-Bash 10-minute cap.
    """
    release_prepare(c, version)
    release_finalize(c, version)


@task(name="release-prepare")
def release_prepare(c: Context, version: str) -> None:
    """Phase 1 of the release.

    Pre-flight → tests → perf → bump → commit → tag → push → GitHub
    Release. Fits comfortably in 5–7 min on a quiet machine. Sub-agents can run
    this in the foreground with the harness's default Bash timeout.
    Pushing the tag triggers the `Publish to PyPI` workflow
    asynchronously; pushing main triggers the RTD `latest` build
    asynchronously. Both finish independently of this task — wait for
    them via ``release-finalize``.

    Pre-flight requirements (all enforced):
      - On `main` branch.
      - Working tree clean (vendored-submodule drift in either
        lowercase ` m vendor/...` or capital ` M vendor/...` form is
        tolerated; everything else rejects).
      - HEAD == origin/main (no unpushed commits).
      - Tag `vX.Y.Z` not already on origin.
      - `READTHEDOCS_TOKEN` available — exported or in `.env` (this
        phase doesn't use the token, but rejecting now means we don't
        push a release and then discover the token is missing in
        finalize).

    Pipeline:
      1. Full default test suite (`pytest` parallel, perf-excluded).
      2. Perf regression gates (serial).
      3. Bump pyproject.toml + src/secantus/__init__.py + uv.lock.
      4. Commit, annotate-tag, push commit + tag (combined push).
      5. Create a GitHub Release for `vX.Y.Z` with auto-generated
         notes (marked pre-release for `aN`/`bN`/`rcN` versions).
      6. Activate the RTD `vX.Y.Z` slug (best-effort) so its build
         runs concurrent with the GitHub `Publish to PyPI` workflow
         rather than after it. Failure here is non-fatal; finalize
         retries the activation idempotently.
    """
    if not _VERSION_RE.match(version):
        raise SystemExit(f"version {version!r} doesn't match X.Y.Z[aN|bN|rcN]")
    _ensure_main_branch_clean()
    _ensure_in_sync_with_origin()
    _ensure_tag_unused(version)
    _ensure_rtd_token()

    print("==> [1/5] Full default test suite")
    c.run("uv run python -m pytest", pty=True)
    print("==> [2/5] Perf regression gates")
    c.run(
        "uv run python -m pytest -p no:xdist -o addopts= -m perf tests/test_perf_regression.py",
        pty=True,
    )

    print(f"==> [3/5] Bumping version files to {version}")
    _bump_version_files(version)
    c.run("uv lock", pty=True)

    print(f"==> [4/6] Committing + tagging v{version}")
    c.run("git add pyproject.toml src/secantus/__init__.py uv.lock", pty=True)
    # If the version is already at ``version`` on HEAD (e.g. because a
    # parallel-session merge bumped it), the ``git add`` stages nothing
    # and ``git commit`` would abort with "nothing to commit". Detect
    # that case and skip the commit — the tag still goes on HEAD which
    # already carries the right version.
    staged = c.run("git diff --cached --quiet", warn=True, hide=True)
    if staged.return_code == 0:
        print(f"    version already at {version} on HEAD; skipping release commit")
    else:
        c.run(f'git commit -m "Release v{version}"', pty=True)
    c.run(f'git tag -a v{version} -m "Release v{version}"', pty=True)
    # Combine the branch and tag pushes into one network round-trip.
    # The publish workflow still fires on the tag ref; nothing else
    # depends on the order of branch-then-tag.
    c.run(f"git push origin main v{version}", pty=True)

    print(f"==> [5/6] Creating GitHub Release v{version}")
    # Pre-release if the version has an `aN` / `bN` / `rcN` suffix.
    is_prerelease = bool(re.search(r"[abc]\d+$|rc\d+$", version))
    cmd = (
        f"gh release create v{version} "
        f"--title 'v{version}' "
        f"--generate-notes "
        f"--target $(git rev-parse HEAD)"
    )
    if is_prerelease:
        cmd += " --prerelease"
    c.run(cmd, pty=True)

    # Activate the RTD slug as early as possible so its build runs
    # concurrent with the GitHub `Publish to PyPI` workflow rather
    # than after it. Best-effort: if the RTD API errors here, finalize
    # will retry — better to push the release than to abort prepare
    # over a transient RTD blip.
    print(f"==> [6/6] Activating RTD `v{version}` slug for early build")
    try:
        _activate_rtd_version(version, _ensure_rtd_token())
    except SystemExit as e:
        print(f"    warning: RTD activate failed in prepare ({e}); finalize will retry")

    print(
        f"\nv{version} prepared, tag pushed, GitHub Release created, RTD build queued.\n"
        f"Run `invoke release-finalize {version}` next to wait for the\n"
        f"publish workflow + PyPI + RTD propagation."
    )


@task(name="release-finalize")
def release_finalize(c: Context, version: str) -> None:
    """Phase 2 of the release.

    Poll publish workflow → PyPI → RTD `latest` → activate `vX.Y.Z`
    slug → poll its build → PATCH RTD `default_version`.

    Polling can run for 15–25 min in the worst case (publish workflow
    builds wheels for cp310-cp313 across 4 platforms; RTD compiles
    WiredTiger from source twice — once for `latest`, once for the
    tag). Sub-agents must call this with ``run_in_background=true``
    on the Bash invocation to escape the harness's 10-min per-call
    cap; foreground in a developer's shell is fine.

    Idempotent: every step short-circuits if the desired state is
    already true (publish workflow already concluded, PyPI already
    lists the version, RTD build already finished, version already
    active, `default_version` already set). Safe to re-run after any
    timeout or interruption.

    Pre-flight requirements:
      - Tag `vX.Y.Z` exists on origin (the prepare phase pushed it).
      - `READTHEDOCS_TOKEN` available.

    Pipeline:
      6. Wait for GitHub `Publish to PyPI` workflow to succeed.
      7. Wait for PyPI to list the new version.
      8. Wait for RTD `latest` to publish a successful build of the
         release commit.
      9. Activate the `vX.Y.Z` slug on RTD and wait for its build.
     10. Set RTD's `default_version` to `vX.Y.Z`.
    """
    if not _VERSION_RE.match(version):
        raise SystemExit(f"version {version!r} doesn't match X.Y.Z[aN|bN|rcN]")
    rtd_token = _ensure_rtd_token()
    commit = _resolve_tag_commit(version)

    print(f"==> [6/10] Waiting for GitHub `Publish to PyPI` workflow (commit {commit[:7]})")
    _wait_for_publish_workflow(commit)
    print(f"==> [7/10] Waiting for PyPI to list {version}")
    _wait_for_pypi_version(version)
    print(f"==> [8/10] Waiting for RTD `latest` to build commit {commit[:7]}")
    _wait_for_rtd_build(commit)
    print(f"==> [9/10] Activating + building RTD `v{version}`")
    _activate_rtd_version(version, rtd_token)
    _wait_for_rtd_tag_build(version, rtd_token)
    print(f"==> [10/10] Setting RTD `default_version` to `v{version}`")
    _set_rtd_default_version(version, rtd_token)

    print(f"\nv{version} released; GitHub Release, PyPI, and RTD up to date.")


def _resolve_tag_commit(version: str) -> str:
    """Resolve the commit SHA for ``vX.Y.Z`` on origin.

    Used by ``release-finalize`` to find the release commit when re-run
    later (after any ``main`` HEAD drift). The annotated tag's target
    is the release commit itself, regardless of what's on ``main`` now.
    """
    out = subprocess.run(
        ["git", "rev-parse", f"v{version}^{{commit}}"],
        capture_output=True,
        text=True,
    )
    if out.returncode != 0:
        # Fall back to the remote ref so finalize works even if the
        # local tag was pruned.
        out = subprocess.run(
            ["git", "ls-remote", "origin", f"refs/tags/v{version}^{{}}"],
            capture_output=True,
            text=True,
            check=True,
        )
        line = out.stdout.strip().split("\n", 1)[0]
        if not line:
            raise SystemExit(
                f"tag v{version} not found on origin — "
                f"run `invoke release-prepare {version}` first."
            )
        return line.split()[0]
    return out.stdout.strip()


def _ensure_main_branch_clean() -> None:
    branch = subprocess.run(
        ["git", "branch", "--show-current"],
        capture_output=True,
        text=True,
        check=True,
    ).stdout.strip()
    if branch != "main":
        raise SystemExit(f"release must run on main; on {branch!r}")
    status = subprocess.run(
        ["git", "status", "--porcelain"],
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    # Vendored-submodule drift comes in two flavours, both tolerated:
    #   " m vendor/foo" — modified content inside the submodule (build-time
    #     WiredTiger patching, etc.).
    #   " M vendor/foo" — submodule HEAD shifted because a parallel worktree
    #     pulled or updated the submodule SHA.
    # Neither goes into the release commit (the task only `git add`s
    # pyproject.toml + __init__.py + uv.lock), so they're safe to ignore.
    # Anything else is uncommitted work the release would either include
    # or shadow — reject it.
    bad = [
        line
        for line in status.splitlines()
        if line and not (line.startswith((" m ", " M ")) and "vendor/" in line)
    ]
    if bad:
        raise SystemExit("working tree has uncommitted changes:\n" + "\n".join(bad))


def _ensure_in_sync_with_origin() -> None:
    subprocess.run(["git", "fetch", "origin"], check=True, capture_output=True)
    head = subprocess.run(
        ["git", "rev-parse", "HEAD"],
        capture_output=True,
        text=True,
        check=True,
    ).stdout.strip()
    origin = subprocess.run(
        ["git", "rev-parse", "origin/main"],
        capture_output=True,
        text=True,
        check=True,
    ).stdout.strip()
    if head != origin:
        raise SystemExit(
            f"local main ({head[:7]}) is not in sync with origin/main "
            f"({origin[:7]}) — push or pull first."
        )


def _ensure_tag_unused(version: str) -> None:
    out = subprocess.run(
        ["git", "ls-remote", "--tags", "origin", f"v{version}"],
        capture_output=True,
        text=True,
        check=True,
    ).stdout.strip()
    if out:
        raise SystemExit(f"tag v{version} already exists on origin — pick a new version.")


def _bump_version_files(version: str) -> None:
    py = pathlib.Path("pyproject.toml")
    init = pathlib.Path("src/secantus/__init__.py")
    py.write_text(
        re.sub(
            r'^version = "[^"]+"',
            f'version = "{version}"',
            py.read_text(),
            count=1,
            flags=re.MULTILINE,
        )
    )
    init.write_text(
        re.sub(
            r'^__version__ = "[^"]+"',
            f'__version__ = "{version}"',
            init.read_text(),
            count=1,
            flags=re.MULTILINE,
        )
    )


def _wait_for_publish_workflow(commit: str, *, timeout_s: int = 1200) -> None:
    deadline = time.monotonic() + timeout_s
    last = ""
    while time.monotonic() < deadline:
        out = subprocess.run(
            [
                "gh",
                "run",
                "list",
                "--workflow=publish.yml",
                f"--commit={commit}",
                "--json=status,conclusion,databaseId",
                "--limit=1",
            ],
            capture_output=True,
            text=True,
            check=True,
        ).stdout
        runs = json.loads(out or "[]")
        if not runs:
            line = "    no publish run for this commit yet; waiting"
        else:
            r = runs[0]
            conc = r.get("conclusion") or ""
            line = f"    run {r['databaseId']}: status={r['status']} conclusion={conc}"
            if r["status"] == "completed":
                if r.get("conclusion") == "success":
                    print(line)
                    return
                raise SystemExit(
                    f"publish workflow {r['databaseId']} concluded {r.get('conclusion')!r}"
                )
        if line != last:
            print(line)
            last = line
        time.sleep(20)
    raise SystemExit(f"timed out after {timeout_s}s waiting for publish workflow")


def _wait_for_pypi_version(version: str, *, timeout_s: int = 600) -> None:
    url = "https://pypi.org/pypi/SecantusDB/json"
    deadline = time.monotonic() + timeout_s
    while time.monotonic() < deadline:
        try:
            with urllib.request.urlopen(url, timeout=15) as resp:
                data = json.load(resp)
        except Exception as e:
            print(f"    PyPI API error: {e}; retrying")
            time.sleep(20)
            continue
        latest = data["info"]["version"]
        if version in data.get("releases", {}):
            print(f"    PyPI lists {version} (info.version={latest})")
            return
        print(f"    PyPI does not list {version} yet (info.version={latest}); waiting")
        time.sleep(20)
    raise SystemExit(f"timed out after {timeout_s}s waiting for PyPI to list {version}")


def _wait_for_rtd_build(commit: str, *, timeout_s: int = 900) -> None:
    url = "https://readthedocs.org/api/v3/projects/secantusdb/builds/?limit=5"
    deadline = time.monotonic() + timeout_s
    last = ""
    while time.monotonic() < deadline:
        try:
            with urllib.request.urlopen(url, timeout=15) as resp:
                data = json.load(resp)
        except Exception as e:
            print(f"    RTD API error: {e}; retrying")
            time.sleep(30)
            continue
        match = next(
            (b for b in data.get("results", []) if (b.get("commit") or "").startswith(commit[:12])),
            None,
        )
        if match is None:
            line = f"    no RTD build found for {commit[:7]} yet; waiting"
        else:
            state = match["state"]["code"]
            success = match.get("success")
            line = f"    build {match['id']}: state={state} success={success}"
            if state == "finished":
                if success:
                    print(line)
                    return
                raise SystemExit(f"RTD build {match['id']} for {commit[:7]} failed")
        if line != last:
            print(line)
            last = line
        time.sleep(30)
    raise SystemExit(f"timed out after {timeout_s}s waiting for RTD build of {commit[:7]}")


_RTD_PROJECT_API = "https://readthedocs.org/api/v3/projects/secantusdb"


def _ensure_rtd_token() -> str:
    """Pre-flight: require READTHEDOCS_TOKEN so the post-publish RTD admin
    operations (activate version, set default_version) can run.

    Resolution order:
      1. ``READTHEDOCS_TOKEN`` already in the process env (e.g. set in
         the user's shell rc).
      2. ``READTHEDOCS_TOKEN=…`` line in a project-root ``.env`` file
         (gitignored). This is the recommended on-disk store.
    """
    token = os.environ.get("READTHEDOCS_TOKEN")
    if not token:
        token = _read_dotenv_var("READTHEDOCS_TOKEN")
    if not token:
        raise SystemExit(
            "READTHEDOCS_TOKEN is required for the release task — without it,\n"
            "RTD's default version stays pinned to whatever it was before this\n"
            "release. Mint one (read+write) at\n"
            "    https://app.readthedocs.org/accounts/tokens/\n"
            "and either export it in your shell or put `READTHEDOCS_TOKEN=…`\n"
            "into a `.env` file at the repo root (which is gitignored)."
        )
    return token


def _read_dotenv_var(key: str) -> str | None:
    """Tiny ``.env`` parser: ``KEY=VALUE`` lines, optional surrounding
    quotes, ``#`` comments. No interpolation, no exports — that would
    duplicate python-dotenv for one variable."""
    env_path = pathlib.Path(".env")
    if not env_path.is_file():
        return None
    for raw in env_path.read_text(encoding="utf-8").splitlines():
        line = raw.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        k, _, v = line.partition("=")
        if k.strip() != key:
            continue
        v = v.strip()
        if len(v) >= 2 and v[0] == v[-1] and v[0] in ("'", '"'):
            v = v[1:-1]
        return v or None
    return None


def _rtd_request(method: str, path: str, token: str, body: dict | None = None) -> dict:
    """Issue a single RTD API v3 request and return the parsed JSON body.

    `path` is appended to the project endpoint (e.g. ``""`` for the
    project itself, ``"/versions/v0.3.0a4/"`` for a version). RTD
    endpoints expect a trailing slash.
    """
    url = _RTD_PROJECT_API + path
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(
        url,
        data=data,
        method=method,
        headers={
            "Authorization": f"Token {token}",
            "Content-Type": "application/json",
            "Accept": "application/json",
        },
    )
    with urllib.request.urlopen(req, timeout=20) as resp:
        payload = resp.read()
    if not payload:
        return {}
    return json.loads(payload)


def _activate_rtd_version(version: str, token: str) -> None:
    """Set the `vX.Y.Z` slug to active so RTD builds it. RTD auto-queues
    a build when a version flips to active=True."""
    path = f"/versions/v{version}/"
    try:
        _rtd_request("PATCH", path, token, body={"active": True, "hidden": False})
    except urllib.error.HTTPError as e:
        body = e.read().decode(errors="replace")
        raise SystemExit(
            f"failed to activate RTD version v{version}: HTTP {e.code} {e.reason}\n{body}"
        ) from e
    print(f"    activated RTD version v{version}")


def _wait_for_rtd_tag_build(version: str, token: str, *, timeout_s: int = 900) -> None:
    """Poll RTD for the most recent build of the `vX.Y.Z` slug until it
    finishes successfully. Activating a version triggers a build, but the
    api may take a few seconds to register it — first iterations may
    legitimately find nothing."""
    deadline = time.monotonic() + timeout_s
    last = ""
    while time.monotonic() < deadline:
        try:
            data = _rtd_request("GET", f"/versions/v{version}/builds/?limit=1", token)
        except Exception as e:
            print(f"    RTD API error: {e}; retrying")
            time.sleep(30)
            continue
        builds = data.get("results", [])
        if not builds:
            line = f"    no build for v{version} yet; waiting"
        else:
            b = builds[0]
            state = b["state"]["code"]
            success = b.get("success")
            line = f"    v{version} build {b['id']}: state={state} success={success}"
            if state == "finished":
                if success:
                    print(line)
                    return
                raise SystemExit(f"RTD build {b['id']} for v{version} failed")
        if line != last:
            print(line)
            last = line
        time.sleep(30)
    raise SystemExit(f"timed out after {timeout_s}s waiting for RTD build of v{version}")


def _set_rtd_default_version(version: str, token: str) -> None:
    """PATCH the project's `default_version` so the bare RTD URL serves
    `v{version}` rather than the previous default (typically `stable` or
    `latest`)."""
    try:
        _rtd_request("PATCH", "/", token, body={"default_version": f"v{version}"})
    except urllib.error.HTTPError as e:
        body = e.read().decode(errors="replace")
        raise SystemExit(
            f"failed to set RTD default_version=v{version}: HTTP {e.code} {e.reason}\n{body}"
        ) from e
    print(f"    RTD default_version set to v{version}")
