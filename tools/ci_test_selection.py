"""Print the test files the ``test`` CI job runs, one per line.

The Python servers are fully deprecated (Joe, 2026-10-20): CI no longer tests
them, and the package is no longer published to PyPI. This picks the rest of
``tests/``, by a rule rather than a hand-kept list, so a new file lands on the
right side without anyone remembering to add it:

* A file that imports the ``secantus`` package is a PYTHON-SERVER test and is
  left out. What is lost: every test of the Python MongoDB and PostgreSQL
  servers, their engines and their admin UI. That is the decision.
  The exception is the repo's own dev tooling that happens to live in the
  package (``_TOOLING_MODULES``): the ops board and its job runner.
* Everything else runs: tooling, gauge plumbing, docs and backlog checks,
  and the Rust-server tests that need no build (source scans, version
  checks). A Rust-server test that needs a built server skips in this job,
  which builds none; it runs in the ``storage-engine`` and ``pg-oracle`` jobs,
  and ``tests/test_ci_runs_rust_server_tests.py`` checks that every file gated
  on the server extension is named in one of them.

Usage: ``uv run python -m pytest $(uv run python tools/ci_test_selection.py)``.
"""

from __future__ import annotations

import pathlib
import re
import sys

REPO = pathlib.Path(__file__).resolve().parent.parent

_IMPORT = re.compile(r"^\s*(?:from|import)\s+(secantus(?:\.[\w.]+)?)\b", re.MULTILINE)
_TOOLING_MODULES = ("secantus.opsboard", "secantus.jobkit")


def is_python_server_test(source: str) -> bool:
    modules = _IMPORT.findall(source)
    return any(not m.startswith(_TOOLING_MODULES) for m in modules)


def selected(tests_dir: pathlib.Path = REPO / "tests") -> list[pathlib.Path]:
    out = []
    for path in sorted(tests_dir.rglob("test_*.py")):
        if not is_python_server_test(path.read_text(encoding="utf-8")):
            out.append(path)
    return out


def main() -> int:
    paths = selected()
    if not paths:
        print("ci_test_selection: selected no tests", file=sys.stderr)
        return 1
    for path in paths:
        print(path.relative_to(REPO).as_posix())
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
