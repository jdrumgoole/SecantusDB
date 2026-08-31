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
"""

from __future__ import annotations

import re
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
WORKFLOW = REPO / ".github" / "workflows" / "test.yml"
TESTS = REPO / "tests"

# Files deliberately NOT run in that job, each with the reason. Empty today:
# every gated file is wired in. A new entry here is a decision, and needs to say
# why -- "it is slow" is a reason, "it was forgotten" is not.
DELIBERATELY_NOT_IN_CI: dict[str, str] = {}


SELF = Path(__file__).name


def _gated_files() -> list[str]:
    """Test files that skip unless the embedded Rust server is built.

    This file is excluded: it QUOTES the idiom in its own docstring to explain
    it, which the first version of this scan read as a use of it and flagged
    the scanner itself.
    """
    out = []
    for path in sorted(TESTS.glob("test_*.py")):
        if path.name == SELF:
            continue
        text = path.read_text(encoding="utf-8")
        if 'importorskip("_secantus_server")' in text:
            out.append(path.name)
    return out


def test_the_gate_pattern_still_matches_something() -> None:
    """Guard against the scan silently matching nothing if the idiom changes."""
    assert _gated_files(), "no _secantus_server-gated test files found — has the idiom moved?"


def test_every_rust_server_test_file_runs_in_ci() -> None:
    workflow = WORKFLOW.read_text(encoding="utf-8")
    missing = [
        name
        for name in _gated_files()
        if name not in DELIBERATELY_NOT_IN_CI
        and not re.search(rf"tests/{re.escape(name)}\b", workflow)
    ]
    assert not missing, (
        "these files skip in every CI lane and are not named in the job that builds "
        "_secantus_server, so they run nowhere:\n  "
        + "\n  ".join(missing)
        + "\n\nAdd them to the 'Storage + Rust-server smoke tests' step in "
        ".github/workflows/test.yml, or record a reason in DELIBERATELY_NOT_IN_CI."
    )


def test_exclusions_all_carry_a_reason() -> None:
    for name, reason in DELIBERATELY_NOT_IN_CI.items():
        assert reason.strip(), f"{name} is excluded from CI with no reason given"
        assert (TESTS / name).exists(), f"{name} is excluded but no longer exists — drop the entry"
