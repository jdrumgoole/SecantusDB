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
        Task("py-test", "Test suite", ["test"], "test", "Full pytest suite."),
        Task("py-gate", "Pre-commit gate", ["py-gate"], "test", "Full Python gate."),
        Task("py-perf", "Perf regression", ["perf"], "test", "Perf gates (serial)."),
        Task("py-lint", "Lint", ["lint"], "test", "ruff check + format --check."),
        Task(
            "py-gauge",
            "pymongo gauge",
            ["validate", "--server", "python"],
            "test",
            "pymongo conformance gauge.",
        ),
        Task(
            "py-release-prepare",
            "release-prepare",
            ["release-prepare"],
            "release",
            "Bump, tag, push (needs a version).",
            confirm=True,
        ),
        Task(
            "py-release-finalize",
            "release-finalize",
            ["release-finalize"],
            "release",
            "Poll publish workflow → PyPI (needs a version).",
            confirm=True,
        ),
    ],
)

RUST = Target(
    key="rust",
    name="Rust server",
    subtitle="secantusd-rs binary + _secantus_server · secantusdb-v tags",
    tasks=[
        Task("rs-test", "cargo test", ["rust-test"], "test", "fmt/clippy/tests."),
        Task("rs-gate", "Pre-commit gate", ["rust-gate"], "test", "Full Rust gate."),
        Task("rs-parity", "Parity suite", ["rust-parity"], "test", "Engine parity."),
        Task(
            "rs-build",
            "Build core wheel",
            ["rust-build"],
            "build",
            "abi3 core wheel.",
        ),
        Task(
            "rs-binary",
            "Build binary",
            ["rust-binary-build"],
            "build",
            "Standalone secantusd binary.",
        ),
        Task(
            "rs-gauge",
            "pymongo gauge (rust)",
            ["validate", "--server", "rust"],
            "test",
            "R8 conformance gate.",
        ),
        Task(
            "rs-bump",
            "Bump crates",
            ["rust-bump"],
            "release",
            "Lockstep crate version bump.",
            confirm=True,
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
        ),
        Task(
            "pg-slt",
            "sqllogictest",
            ["validate-slt"],
            "test",
            "sqllogictest corpus.",
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


__all__ = ["Task", "Target", "TARGETS", "target", "resolve_task", "PYTHON", "RUST", "PG"]
