"""Declarative catalog of the managed servers and their invoke tasks.

Data-driven so the UI never hardcodes a capability table that silently goes
stale — a new gauge or target is a one-line edit here. Each ``Task`` names an
invoke task plus the argv passed to ``./inv`` (via jobkit).
"""

from __future__ import annotations

from dataclasses import dataclass, field


@dataclass(frozen=True)
class Task:
    key: str  # stable id, unique across the catalog
    label: str  # human label for the button
    argv: list[str]  # what jobkit runs: ./inv <argv...>
    phase: str  # "build" | "test" | "release"
    blurb: str = ""
    confirm: bool = False  # outward-facing / irreversible → typed confirmation
    # Ordered sub-phase names for the progress stepper. Only meaningful for
    # multi-step tasks that emit ``==> [k/N] label`` markers (the gates). Empty
    # → progress falls back to the pytest % bar or an indeterminate bar.
    phase_labels: list[str] = field(default_factory=list)
    # When True the UI shows a parallelism input that becomes ``--jobs N`` (for
    # validate-all, which dispatches the gauges over a thread pool).
    jobs_option: bool = False
    default_jobs: int = 4  # matches validate-all's default; CLAUDE.md caps at 4
    # Long-form explanation shown in the task's info dialog.
    detail: str = ""
    # ROUGH order-of-magnitude fallback (seconds) shown only until this machine
    # has recorded a successful run; the UI then quotes the observed median
    # instead and says which it is. Never presented as measured.
    est_seconds: int = 0


@dataclass(frozen=True)
class Target:
    key: str  # "python" | "rust" | "pg"
    name: str
    subtitle: str
    tasks: list[Task] = field(default_factory=list)

    def by_phase(self, phase: str) -> list[Task]:
        return [t for t in self.tasks if t.phase == phase]


PYTHON = Target(
    key="python",
    name="Python server",
    subtitle="pure-Python SecantusDBServer · PyPI SecantusDB",
    tasks=[
        Task(
            "py-test",
            "Test suite",
            ["test"],
            "test",
            "Full pytest suite.",
            detail=(
                "Runs the whole pytest suite for the Python server, in parallel "
                "via pytest-xdist (-n auto). Tests use real on-disk WiredTiger, "
                "but in fast test-storage mode (durable=False), so the "
                "checkpoint-durability path is NOT covered here — CI's "
                "SECANTUS_FORCE_DURABLE lane covers that. Excludes the perf "
                "markers, which the Perf task runs separately."
            ),
            est_seconds=300,
        ),
        Task(
            "py-gate",
            "Pre-commit gate",
            ["py-gate"],
            "test",
            "Full Python gate.",
            phase_labels=["Lint", "Tests", "Perf"],
            detail=(
                "The full pre-commit gate for Python-server work, in three "
                "phases: (1) Lint — ruff check plus ruff format --check; "
                "(2) Tests — the full pytest suite; (3) Perf — the "
                "perf-regression gate. This is what must be green before "
                "committing changes under src/secantus/. Each phase reports as "
                "a step in the progress bar above."
            ),
            est_seconds=720,
        ),
        Task(
            "py-perf",
            "Perf regression",
            ["perf"],
            "test",
            "Perf gates (serial).",
            detail=(
                "Runs tests/test_perf_regression.py serially (no xdist) with "
                "the perf marker enabled. This is the only suite that stays on "
                ":memory: storage, because the gates compare against fixed "
                "in-memory baselines that on-disk variance would invalidate. "
                "Run it when an optimisation is meant to move a number."
            ),
            est_seconds=240,
        ),
        Task(
            "py-lint",
            "Lint",
            ["lint"],
            "test",
            "ruff check + format --check.",
            detail=(
                "Runs ruff check and ruff format --check over src/ and tests/. "
                "Fast, and the usual reason CI goes red on an otherwise good "
                "change. Pure-Python — needs no WiredTiger build, so it works "
                "even in a freshly-created worktree."
            ),
            est_seconds=25,
        ),
        Task(
            "py-gauge",
            "pymongo gauge",
            ["validate", "--server", "python"],
            "test",
            "pymongo conformance gauge.",
            detail=(
                "Runs pymongo's own vendored test suite — unmodified — against "
                "an embedded SecantusDB. This is the headline 'MongoDB "
                "compatibility' number: behaviour is correct when a pymongo "
                "client can't tell SecantusDB apart from a real mongod. "
                "Generates docs/validation-report.md with a per-category "
                "pass/fail/skip breakdown."
            ),
            est_seconds=600,
        ),
        Task(
            "py-gauge-all",
            "All gauges",
            ["validate-all", "--server", "python"],
            "test",
            "All 13 driver gauges (parallel; needs each driver's toolchain).",
            jobs_option=True,
            detail=(
                "Runs ALL thirteen driver-conformance gauges against the Python "
                "server: pymongo, pymongo-async, Go, Node, Java, Kotlin, Ruby, "
                "Rust, PHP (library + extension), C, C++ and C#/.NET — each the "
                "driver's own unmodified upstream suite. The other-language "
                "gauges catch wire-protocol bugs pymongo's permissive client "
                "misses. Requires each driver's toolchain to be installed "
                "locally; a missing toolchain fails that gauge only. Gauges are "
                "dispatched over a thread pool — set the parallelism with the "
                "adjacent field (4 or fewer is recommended; above that, "
                "CPU contention makes timing-sensitive gauges flake). This is "
                "by far the longest task here."
            ),
            est_seconds=3600,
        ),
        Task(
            "py-release-prepare",
            "release-prepare",
            ["release-prepare"],
            "release",
            "Bump, tag, push (needs a version).",
            confirm=True,
            detail=(
                "Phase 1 of the PyPI release: pre-flight checks, the full test "
                "suite, perf gates, changelog collation, the version bump, then "
                "tag and push. IRREVERSIBLE and outward-facing — it pushes a "
                "vX.Y.Z tag that triggers the publish workflow. Needs a version "
                "argument. Not startable from this dashboard; the confirm-gated "
                "Release page owns it."
            ),
            est_seconds=900,
        ),
        Task(
            "py-release-finalize",
            "release-finalize",
            ["release-finalize"],
            "release",
            "Poll publish workflow → PyPI (needs a version).",
            confirm=True,
            detail=(
                "Phase 2 of the PyPI release: waits for the GitHub 'Publish to "
                "PyPI' workflow to succeed, then waits for PyPI to list the new "
                "version. Idempotent — every step short-circuits if already "
                "true, so it's safe to re-run after a timeout. Polling can run "
                "15–25 minutes because the publish workflow builds wheels "
                "across four platforms."
            ),
            est_seconds=1500,
        ),
    ],
)

RUST = Target(
    key="rust",
    name="Rust server",
    subtitle="secantusd-rs binary + _secantus_server · secantusdb-v tags",
    tasks=[
        Task(
            "rs-test",
            "cargo test",
            ["rust-test"],
            "test",
            "fmt/clippy/tests.",
            detail=(
                "cargo fmt --check, clippy with warnings-as-errors, and the "
                "unit tests across the clean (PyO3-free, non-WiredTiger-linked) "
                "workspace. Does not cover the WiredTiger-linked crates — the "
                "full gate adds those."
            ),
            est_seconds=420,
        ),
        Task(
            "rs-gate",
            "Pre-commit gate",
            ["rust-gate"],
            "test",
            "Full Rust gate.",
            phase_labels=[
                "cargo (clean ws)",
                "wt crate",
                "storage crate",
                "adapter crate",
                "parity",
                "ruff check",
                "ruff format",
                "pytest",
            ],
            detail=(
                "The sequence that must be green before committing Rust work, "
                "in eight phases: the clean-workspace fmt/clippy/test, then "
                "each WiredTiger-linked crate the clean workspace can't cover "
                "(wt, storage, adapter), the leaf-engine parity suites, Python "
                "ruff check + format (the parity tests are Python, so a ruff "
                "slip there would otherwise pass the gate and red CI), and "
                "finally the full Python suite. Each phase shows as a step in "
                "the progress bar. The longest routine gate here."
            ),
            est_seconds=1800,
        ),
        Task(
            "rs-parity",
            "Parity suite",
            ["rust-parity"],
            "test",
            "Engine parity.",
            detail=(
                "Builds the Rust core and runs the leaf-engine parity suites "
                "against it. These pin each Rust engine byte-for-byte to its "
                "pure-Python counterpart over a curated corpus plus randomised "
                "fuzz; the Rust side returns a 'defer to Python' signal for "
                "constructs it can't reproduce exactly (regex, collation, "
                "Decimal128 edges…). This is the oracle that stops the two "
                "engines drifting — extend it first when porting an operator."
            ),
            est_seconds=300,
        ),
        Task(
            "rs-build",
            "Build core wheel",
            ["rust-build"],
            "build",
            "abi3 core wheel.",
            detail=(
                "Builds the abi3 wheel for the Rust core (_secantus_core) into "
                "target/wheels/. That's the reusable pure-Rust engine crate "
                "behind the Rust server and the parity-test oracle; the Python "
                "server does not import it."
            ),
            est_seconds=300,
        ),
        Task(
            "rs-binary",
            "Build binary",
            ["rust-binary-build"],
            "build",
            "Standalone secantusd binary.",
            detail=(
                "Builds the standalone secantusd-rs binary (the Rust MongoDB-"
                "wire server) and prints its path. Statically links the "
                "vendored WiredTiger, so a cold build is slow; incremental "
                "rebuilds are much faster."
            ),
            est_seconds=420,
        ),
        Task(
            "rs-gauge",
            "pymongo gauge (rust)",
            ["validate", "--server", "rust"],
            "test",
            "R8 conformance gate.",
            detail=(
                "Runs pymongo's unmodified vendored suite against the RUST "
                "server (via the embedded _secantus_server handle) instead of "
                "the Python one, writing docs/validation-report-rust-server.md. "
                "This is the R8 conformance gate from the Rust server plan. "
                "Rebuild the embedded extension first so it measures current "
                "code."
            ),
            est_seconds=600,
        ),
        Task(
            "rs-gauge-all",
            "All gauges",
            ["validate-all", "--server", "rust"],
            "test",
            "All 13 driver gauges (parallel; needs each driver's toolchain).",
            jobs_option=True,
            detail=(
                "All thirteen driver-conformance gauges run against the RUST "
                "server (the standalone secantusdb binary) rather than the "
                "Python one. Same coverage and toolchain requirements as the "
                "Python variant; reports get a -rust-server suffix. Set the "
                "parallelism with the adjacent field — 4 or fewer is "
                "recommended. This is by far the longest task here."
            ),
            est_seconds=3600,
        ),
        Task(
            "rs-bump",
            "Bump crates",
            ["rust-bump"],
            "release",
            "Lockstep crate version bump.",
            confirm=True,
            detail=(
                "Bumps every Rust crate (all twelve Cargo.toml plus their "
                "Cargo.lock) in lockstep — the WiredTiger-linked crates are "
                "excluded from the clean workspace and can't inherit a "
                "workspace version, so they're all bumped together. Note a "
                "patch/minor bump resets the beta label to 0. Release-class, so "
                "it's confirm-gated and not startable from this dashboard."
            ),
            est_seconds=60,
        ),
    ],
)

PG = Target(
    key="pg",
    name="PostgreSQL server",
    subtitle="SecantusPGServer · ships with the Python package",
    tasks=[
        Task(
            "pg-psycopg",
            "psycopg gauge",
            ["validate-psycopg"],
            "test",
            "psycopg 3 conformance gauge.",
            detail=(
                "Runs psycopg 3's own vendored test suite against a "
                "SecantusPGServer daemon — the real-driver conformance gauge "
                "for the PostgreSQL wire server, the PG-side counterpart to the "
                "pymongo gauge. The vendored psycopg checkout and the installed "
                "psycopg are pinned to the same version and must stay in "
                "lockstep."
            ),
            est_seconds=480,
        ),
        Task(
            "pg-slt",
            "sqllogictest",
            ["validate-slt"],
            "test",
            "sqllogictest corpus.",
            detail=(
                "Runs the sqllogictest corpus against a SecantusPGServer "
                "daemon. sqllogictest is the standard SQL-engine conformance "
                "corpus: each file is a script of statements and queries with "
                "expected results, so this exercises the SQL engine's "
                "semantics broadly rather than its wire protocol."
            ),
            est_seconds=300,
        ),
    ],
)

TARGETS: list[Target] = [PYTHON, RUST, PG]
_BY_KEY = {t.key: t for t in TARGETS}
_TASK_BY_KEY = {task.key: (t, task) for t in TARGETS for task in t.tasks}


def target(key: str) -> Target | None:
    return _BY_KEY.get(key)


def resolve_task(key: str) -> tuple[Target, Task] | None:
    return _TASK_BY_KEY.get(key)


def find_task_by_argv(argv: list[str]) -> Task | None:
    """Best match for a journal row's argv → catalog Task (for phase labels).

    Prefers an exact argv match (distinguishes ``validate --server python`` from
    ``--server rust``), falling back to the first task sharing argv[0].
    """
    if not argv:
        return None
    for _t, task in _TASK_BY_KEY.values():
        if task.argv == argv:
            return task
    for _t, task in _TASK_BY_KEY.values():
        if task.argv and task.argv[0] == argv[0]:
            return task
    return None


__all__ = [
    "Task",
    "Target",
    "TARGETS",
    "target",
    "resolve_task",
    "find_task_by_argv",
    "PYTHON",
    "RUST",
    "PG",
]
