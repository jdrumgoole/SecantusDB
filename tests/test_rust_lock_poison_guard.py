"""Tripwire against non-poison-tolerant mutex locks in the Rust storage/command
crates.

``self.lock().unwrap()`` on a wire-reachable mutex permanently poisons it the
moment any code holding the lock panics — every later operation on that mutex
then panics too. The whole codebase was swept to
``.lock().unwrap_or_else(|e| e.into_inner())`` (poison-tolerant) once, and the
pattern was reintroduced 23 minutes later by a concurrent commit on the shared
oplog mutex — the exact regression #593 records. This guard makes the regression
a fast, unmissable test failure instead of waiting for the next security review.

Scope: ``crates/secantus-storage`` and ``crates/secantus-commands`` (the crates
whose mutexes guard wire-reachable state). ``#[cfg(test)]`` modules are exempt —
a poisoned lock in a unit test is the test's own crash, not a server wedge.
"""

from __future__ import annotations

import pathlib
import re

_REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent
_CRATES = ("crates/secantus-storage/src", "crates/secantus-commands/src")
_BAD = re.compile(r"\.lock\(\)\.unwrap\(\)")
_TEST_MOD = re.compile(r"^\s*#\[cfg\(test\)\]")


def _offending_lines(path: pathlib.Path) -> list[int]:
    """Line numbers with a bare ``.lock().unwrap()`` outside a ``#[cfg(test)]``
    module. The test-module detection is line-based (a ``#[cfg(test)] mod tests``
    is always the tail of these files), which is sufficient here."""
    hits: list[int] = []
    in_test_mod = False
    for n, line in enumerate(path.read_text().splitlines(), 1):
        if _TEST_MOD.match(line):
            in_test_mod = True
        if in_test_mod:
            continue
        if _BAD.search(line):
            hits.append(n)
    return hits


def test_no_non_poison_tolerant_locks() -> None:
    offenders: dict[str, list[int]] = {}
    for crate_dir in _CRATES:
        root = _REPO_ROOT / crate_dir
        if not root.exists():
            continue
        for rs in root.rglob("*.rs"):
            lines = _offending_lines(rs)
            if lines:
                offenders[str(rs.relative_to(_REPO_ROOT))] = lines
    assert not offenders, (
        "non-poison-tolerant `.lock().unwrap()` found (use "
        "`.lock().unwrap_or_else(|e| e.into_inner())` — see issue #593):\n"
        + "\n".join(f"  {f}: lines {ls}" for f, ls in sorted(offenders.items()))
    )
