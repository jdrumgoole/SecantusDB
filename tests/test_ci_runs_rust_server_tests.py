"""Every Rust-server test file must be named in the job that can run it.

A test file gated on ``importorskip("_secantus_server")`` skips in every
ordinary CI lane, because no other job builds that extension. The one job that
does -- ``storage-engine`` in ``.github/workflows/test.yml`` -- runs a list of
NAMED files rather than the suite, because it installs a minimal environment
(pymongo + pytest + the storage-flavoured wheel) in which collecting all of
``tests/`` would fail on imports.

The consequence, found on 2026-08-31: a file that is not on that list runs
NOWHERE. Six were in that state, two of them data-integrity suites (crash
recovery, cross-server PITR) and one pinning the 76 argument slots fixed that
day. Nothing failed; they simply never ran.

This test makes the list self-maintaining. Add a Rust-server test file and this
fails until the file is either wired into the job or given an explicit reason
below -- so the decision is recorded rather than defaulted.

Three gates count (widened 2026-10-21): the embedded server
(`_secantus_server`), the engine extension (`_secantus_core`), which no
ordinary lane builds either since 2026-10-20, and the standalone binary
(`SECANTUSDB_BIN`). `test_rust_binary_pitr.py` and
`test_rust_group_field_pushdown.py` ran nowhere for weeks behind the last two
while the first version of this scan looked only for the first.
"""

from __future__ import annotations

import re
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
WORKFLOWS = REPO / ".github" / "workflows"
TESTS = REPO / "tests"

#: The ways a test file says it needs a Rust artifact no ordinary lane builds.
GATES = re.compile(r"""importorskip\(\s*["']_secantus_(?:server|core)["']|SECANTUSDB_BIN""")

_PARITY = (
    "a Rust-engine-vs-Python-engine parity suite: a drift detector against "
    "the fully deprecated Python engine, retired from CI on 2026-10-20 (the "
    "reference is mongod, not the other engine)"
)

# Files deliberately NOT run in CI, each with the reason. A new entry here is
# a decision, and needs to say why -- "it is slow" is a reason, "it was
# forgotten" is not.
DELIBERATELY_NOT_IN_CI: dict[str, str] = {
    "test_rust_aggregate_parity.py": _PARITY,
    "test_rust_decimal_parity.py": _PARITY,
    "test_rust_diff_parity.py": _PARITY,
    "test_rust_expressions_parity.py": _PARITY,
    "test_rust_projection_parity.py": _PARITY,
    "test_rust_query_parity.py": _PARITY,
    "test_rust_sortkey_parity.py": _PARITY,
    "test_rust_update_parity.py": _PARITY,
    "test_mongod_differential.py": (
        "drives the deprecated Python server against mongod (2026-10-20); the "
        "Rust MongoDB server's comparison with mongod is tests/backlog/test_rm_*.py"
    ),
}


SELF = Path(__file__).name


def _gated_files() -> list[str]:
    """Test files that skip unless a Rust artifact (`GATES`) is built.

    This file is excluded: it QUOTES the idiom in its own docstring to explain
    it, which the first version of this scan read as a use of it and flagged
    the scanner itself.
    """
    out = []
    for path in sorted(TESTS.glob("test_*.py")):
        if path.name == SELF:
            continue
        text = path.read_text(encoding="utf-8")
        if GATES.search(text):
            out.append(path.name)
    return out


def _workflow_code(path: Path) -> str:
    """A workflow with its YAML comments removed: a comment that MENTIONS a
    file (the build step's note on why it builds the binary) runs nothing, and
    the first version of this scan counted one as running the file."""
    return "\n".join(re.sub(r"(^|\s)#.*", "", ln) for ln in path.read_text("utf-8").splitlines())


def test_the_gate_pattern_still_matches_something() -> None:
    """Guard against the scan silently matching nothing if the idiom changes."""
    gated = _gated_files()
    assert gated, "no Rust-gated test files found — has the idiom moved?"
    # Each gate on its own, so one idiom moving is not hidden by the others.
    for needle in ('"_secantus_server"', '"_secantus_core"', "SECANTUSDB_BIN"):
        assert any(needle in (TESTS / n).read_text(encoding="utf-8") for n in gated), needle


def test_every_rust_server_test_file_runs_in_ci() -> None:
    workflow = "\n".join(_workflow_code(p) for p in sorted(WORKFLOWS.glob("*.yml")))
    missing = [
        name
        for name in _gated_files()
        if name not in DELIBERATELY_NOT_IN_CI
        and not re.search(rf"tests/{re.escape(name)}\b", workflow)
    ]
    assert not missing, (
        "these files skip in every CI lane and are not named in any workflow, so "
        "they run nowhere:\n  "
        + "\n  ".join(missing)
        + "\n\nName them in the job that builds what they need (test.yml: the "
        "'Storage + Rust-server smoke tests' step for _secantus_server, the rust "
        "job's pushdown step for _secantus_core, the standalone-binary step for "
        "SECANTUSDB_BIN), or record a reason in DELIBERATELY_NOT_IN_CI."
    )


def test_exclusions_all_carry_a_reason() -> None:
    for name, reason in DELIBERATELY_NOT_IN_CI.items():
        assert reason.strip(), f"{name} is excluded from CI with no reason given"
        assert (TESTS / name).exists(), f"{name} is excluded but no longer exists — drop the entry"


# --- The Rust PostgreSQL server ---------------------------------------------
#
# The same hole, for the `secantusd-pg` BINARY rather than the extension. A file
# that builds the binary's path (`"secantus-pg" / "target"`), or borrows the
# slice suite's `_Server`, skips wherever the binary is not built -- which is
# every lane but the ones that build it. Found on 2026-10-21: of the 14 tests in
# `test_rust_pgserver_differential.py`, ONE ran in CI, because the workflow
# named a single node of the file and the scan above was looking for the
# extension's idiom only.
#
# A file counts only when a workflow names the WHOLE file. `tests/x.py::one`
# runs one test, and is what left the other thirteen running nowhere.

WORKFLOWS = REPO / ".github" / "workflows"

_PG_GATE = re.compile(r'"secantus-pg"\s*/\s*"target"|from tests\.test_rust_pgserver_slice import')

# Binary-gated files deliberately run in no workflow, each with the reason.
PG_DELIBERATELY_NOT_IN_CI: dict[str, str] = {}


def _pg_gated_files() -> list[str]:
    return sorted(
        path.name
        for path in TESTS.glob("test_*.py")
        if path.name != SELF and _PG_GATE.search(path.read_text(encoding="utf-8"))
    )


def _named_whole(name: str) -> bool:
    pattern = re.compile(rf"tests/{re.escape(name)}(?!::)\b")
    return any(pattern.search(_workflow_code(w)) for w in sorted(WORKFLOWS.glob("*.yml")))


def test_the_pg_gate_pattern_still_matches_something() -> None:
    gated = _pg_gated_files()
    assert "test_rust_pgserver_slice.py" in gated, gated
    assert "test_rust_pgserver_differential.py" in gated, gated


def test_every_rust_pg_server_test_file_runs_in_ci() -> None:
    missing = [
        name
        for name in _pg_gated_files()
        if name not in PG_DELIBERATELY_NOT_IN_CI and not _named_whole(name)
    ]
    assert not missing, (
        "these files need the secantusd-pg binary and no workflow runs the whole file, "
        "so they (or all but the named nodes) run nowhere:\n  "
        + "\n  ".join(missing)
        + "\n\nName the file in the pg-oracle job of .github/workflows/test.yml (or a "
        "scheduled workflow that builds secantusd-pg), or record a reason in "
        "PG_DELIBERATELY_NOT_IN_CI."
    )


def test_pg_exclusions_all_carry_a_reason() -> None:
    for name, reason in PG_DELIBERATELY_NOT_IN_CI.items():
        assert reason.strip(), f"{name} is excluded from CI with no reason given"
        assert (TESTS / name).exists(), f"{name} is excluded but no longer exists — drop the entry"
