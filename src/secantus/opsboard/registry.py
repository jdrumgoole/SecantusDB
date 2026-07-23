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

# --------------------------------------------------------------------------- #
# Driver-conformance gauges.
#
# Thirteen upstream driver suites, run UNMODIFIED against SecantusDB. Declared
# as data (not hand-written Task entries) so the gauge matrix page can render
# every gauge × server combination without a hardcoded capability table.
# --------------------------------------------------------------------------- #


@dataclass(frozen=True)
class GaugeSpec:
    key: str  # short id, e.g. "go"
    label: str  # display name, e.g. "mongo-go-driver"
    task: str  # invoke task name, e.g. "validate-go"
    detail: str
    needs: str = ""  # local toolchain requirement, shown in the dialog
    est_seconds: int = 600


GAUGES: list[GaugeSpec] = [
    GaugeSpec(
        "pymongo",
        "pymongo",
        "validate",
        "pymongo's own vendored test suite, unmodified — the same tests "
        "pymongo's CI runs against a real mongod. This is the honest "
        "'MongoDB compatibility' number. Runs an embedded SecantusDB inside "
        "the pytest process (not a daemon subprocess). Serial by default, "
        "which is how the published figure is measured.",
        needs="none (embedded)",
        est_seconds=600,
    ),
    GaugeSpec(
        "pymongo-async",
        "pymongo (async)",
        "validate-pymongo-async",
        "pymongo's native AsyncMongoClient suite — the async/await wire path "
        "that replaced Motor — against the same embedded server, driven by "
        "pytest-asyncio. Scope mirrors the sync gauge's server-touching set, "
        "restricted to files that have an asynchronous/ variant.",
        needs="none (embedded)",
        est_seconds=600,
    ),
    GaugeSpec(
        "go",
        "mongo-go-driver",
        "validate-go",
        "The Go driver's suite. Go is type-strict where pymongo is permissive "
        "(cursor.id MUST be int64, not int32), so this catches wire-protocol "
        "bugs the pymongo gauge cannot — one of the two strictest gauges. The "
        "Go driver also underpins mongodump/mongorestore, so if it works here "
        "the broader ecosystem does. Known load flake: TestIndexView / "
        "TestChangeStream time out on a saturated machine — run it alone.",
        needs="go 1.21+",
        est_seconds=900,
    ),
    GaugeSpec(
        "node",
        "node-mongodb-native",
        "validate-node",
        "The Node driver's mocha suite (one-time npm install + bundle build). "
        "The include set is restricted to the import-clean subset of unit "
        "tests: v7.2.0 has 68 files using extensionless ESM imports that need "
        "a non-trivial loader chain, and patching their .mocharc would defeat "
        "the unmodified-submodule property — so it trades coverage for honesty.",
        needs="Node.js >= 20",
        est_seconds=900,
    ),
    GaugeSpec(
        "java",
        "mongo-java-driver",
        "validate-java",
        "Runs the driver's own Gradle modules against the daemon via "
        "-Dorg.mongodb.test.uri, then harvests JUnit XML out of the build "
        "tree (leaving the submodule untouched). Include set is :bson:test "
        "(~289 BSON serialization files); the integration modules need a real "
        "replica-set topology. Needs a JDK (javac), not just a JRE, and Gradle "
        "8.12 can't run on JDK 24+ — the runner auto-selects openjdk@17.",
        needs="JDK 17 (javac)",
        est_seconds=1200,
    ),
    GaugeSpec(
        "kotlin",
        "mongo-kotlin-driver",
        "validate-kotlin",
        "The official Kotlin driver, which ships inside the Java monorepo, so "
        "it reuses the same vendored submodule and JDK/Gradle toolchain. "
        "Targets :driver-kotlin-sync:integrationTest rather than :test — the "
        "unit tree is Mockito-mocked and never opens a socket, while the "
        "integration tree exchanges real wire commands.",
        needs="JDK 17 (javac)",
        est_seconds=1200,
    ),
    GaugeSpec(
        "ruby",
        "mongo-ruby-driver",
        "validate-ruby",
        "The Ruby driver's rspec suite, restricted to specs that require the "
        "'lite' spec helper. Mixing in any full-spec_helper file poisons the "
        "run: it triggers a global authorized-client setup that fails SCRAM-256 "
        "against our unauthenticated daemon. Results go to a file, not stdout, "
        "because Mongo::Logger would corrupt JSON capture.",
        needs="Ruby >= 2.7 + bundler",
        est_seconds=900,
    ),
    GaugeSpec(
        "rust",
        "mongo-rust-driver",
        "validate-rust",
        "The Rust driver's test suite against a SecantusDB daemon over "
        "MONGODB_URI. (Note: this is the Rust *driver* as a client — not the "
        "Rust server; use the --server switch to choose which server it "
        "tests against.)",
        needs="cargo / rustc",
        est_seconds=900,
    ),
    GaugeSpec(
        "php-lib",
        "mongo-php-library",
        "validate-php-lib",
        "The high-level mongodb/mongodb PHPUnit suite over the curated "
        "functional directories (Operation / Collection / Database / Command "
        "plus pure-code units). Excludes the cross-driver spec corpus, GridFS "
        "and doc examples — they need orchestration SecantusDB can't provide.",
        needs="PHP >= 8.1 + mongodb ext + composer",
        est_seconds=900,
    ),
    GaugeSpec(
        "php-ext",
        "mongo-php-driver (.phpt)",
        "validate-php-ext",
        "The low-level PECL mongodb C extension wrapping libmongoc — the "
        "strictest wire-protocol gauge alongside Go. Runs .phpt tests against "
        "the ALREADY-INSTALLED extension, so the vendored submodule tag must "
        "match the installed extension version or version-sensitive tests "
        "diverge. Tests self-guard by topology, so RS/transaction/CSFLE "
        "cases skip cleanly.",
        needs="PHP >= 8.1 + mongodb ext (version-matched)",
        est_seconds=600,
    ),
    GaugeSpec(
        "c",
        "mongo-c-driver",
        "validate-c",
        "libmongoc's own test-libmongoc suite, built from source. Along with "
        "Go and the PHP extension, one of the strictest wire-protocol checks — "
        "a C driver makes no allowances for a permissive server.",
        needs="C toolchain + cmake (builds from source)",
        est_seconds=1500,
    ),
    GaugeSpec(
        "cxx",
        "mongo-cxx-driver",
        "validate-cxx",
        "The mongocxx Catch2 test_driver, built from source. NOTE: this gauge "
        "binds port 27017 — mongocxx's tests hard-wire the default URI with no "
        "env override — so it can't share a host with anything else on 27017 "
        "and must stay serial in validate-all.",
        needs="C++ toolchain + cmake (builds from source; uses port 27017)",
        est_seconds=1500,
    ),
    GaugeSpec(
        "dotnet",
        "mongo-csharp-driver",
        "validate-dotnet",
        "The C#/.NET driver's MongoDB.Driver.Tests CRUD-spec suite via "
        "dotnet test. Needs gpg as well as the .NET SDK, because the driver's "
        "Encryption project verifies a downloaded libmongocrypt with gpg at "
        "build time.",
        needs=".NET SDK + gpg",
        est_seconds=1200,
    ),
]

SERVERS: list[tuple[str, str]] = [("python", "Python server"), ("rust", "Rust server")]


def _gauge_task(spec: GaugeSpec, server: str) -> Task:
    needs = f"\n\nRequires locally: {spec.needs}." if spec.needs else ""
    return Task(
        key=f"gauge-{spec.key}-{server}",
        label=spec.label,
        argv=[spec.task, "--server", server],
        phase="test",
        blurb=f"{spec.label} gauge against the {server} server.",
        detail=f"{spec.detail}{needs}",
        est_seconds=spec.est_seconds,
    )


GAUGE_TASKS: list[Task] = [
    _gauge_task(spec, server) for spec in GAUGES for server, _name in SERVERS
]


def gauge_task(spec_key: str, server: str) -> Task | None:
    return _TASK_BY_KEY.get(f"gauge-{spec_key}-{server}", (None, None))[1]


TARGETS: list[Target] = [PYTHON, RUST, PG]
_BY_KEY = {t.key: t for t in TARGETS}
_TASK_BY_KEY: dict[str, tuple[Target | None, Task]] = {
    task.key: (t, task) for t in TARGETS for task in t.tasks
}
# Gauge tasks are startable/resolvable too, but live on the gauge matrix page
# rather than the dashboard cards (26 combinations would drown the cards).
_TASK_BY_KEY.update({task.key: (None, task) for task in GAUGE_TASKS})


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
