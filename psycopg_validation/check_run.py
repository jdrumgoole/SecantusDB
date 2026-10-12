"""Say whether a psycopg gauge run COMPLETED, and compare a TLS run with a
plain one.

A run that stopped part-way is a failure and never a pass: pytest-json-report
writes its file at sessionfinish, so a killed run leaves no report, or one
whose collected count exceeds the outcomes it holds. That check is the first
thing this does for every report it is given.

Usage::

    python -m psycopg_validation.check_run .validation/psycopg-raw-rust.json
    python -m psycopg_validation.check_run PLAIN.json --tls TLS.json
    python -m psycopg_validation.check_run PLAIN.json --tls TLS.json --shared-runner

With ``--tls`` the second report is held to the first: a test that fails over
TLS and passes in the clear is the server's TLS path answering differently,
and that is what the exit code reports. A test that fails in BOTH is printed
and does not fail the comparison; it is the plain gauge's finding.

``--shared-runner`` is for a hosted CI runner. There the tests in
`SHARED_RUNNER_NOISE` fail in one run and pass in the next whatever the
transport, so one failing over TLS and passing in the clear says nothing
about TLS. They are still printed, and they do not fail the comparison.
Without the flag every test counts, which is how a developer's machine runs
it.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

_BAD = ("failed", "error")

#: Tests whose verdict is a measurement taken in the CLIENT process, and
#: which a hosted runner fails now and then on its own: (what the test id
#: holds, why). Measured on `ubuntu-latest`, 2026-10-12, two runs of the suite
#: in the clear against one tree: 6 failures, then 31, where a Mac has none.
#:
#: What leaving them out of `--shared-runner` loses: a change that delays or
#: drops NOTIFY delivery over TLS only would not fail that lane. The same
#: tests are held to the comparison wherever the flag is not given.
SHARED_RUNNER_NOISE: tuple[tuple[str, str], ...] = (
    (
        "::test_notify",
        "asserts a NOTIFY sent 0.25 s in arrives within 25 to 50 ms of then; the runner's"
        " are 55 to 80 ms late in either transport",
    ),
    ("::test_notifies", "the same measurement, from a second thread"),
    (
        "::test_leak",
        "counts Python objects in the client before and after; 22 of its 72 cases"
        " failed in one run in the clear and none in the run before",
    ),
)


class Truncated(Exception):
    """The run did not account for every test it started."""


def load(path: Path) -> dict[str, str]:
    """A report's outcome per test id, or `Truncated` if the run stopped short."""
    if not path.exists():
        raise Truncated(f"{path}: no report -- the run did not reach sessionfinish")
    report = json.loads(path.read_text())
    summary = report.get("summary", {})
    tests = report.get("tests", [])
    started = summary.get("collected", 0) - summary.get("deselected", 0)
    if started <= 0:
        raise Truncated(f"{path}: the run started no tests")
    if len(tests) != started:
        raise Truncated(f"{path}: started {started} tests, {len(tests)} reported a result")
    return {t["nodeid"]: t["outcome"] for t in tests}


def describe(name: str, outcomes: dict[str, str]) -> None:
    counts: dict[str, int] = {}
    for outcome in outcomes.values():
        counts[outcome] = counts.get(outcome, 0) + 1
    print(f"{name}: {len(outcomes)} tests reported -- {dict(sorted(counts.items()))}")


def tls_only_failures(plain: dict[str, str], tls: dict[str, str]) -> list[str]:
    """Tests that fail over TLS and do not fail in the clear."""
    return sorted(t for t, o in tls.items() if o in _BAD and plain.get(t) not in _BAD)


def noise_reason(test: str) -> str | None:
    """Why `test` is not held to the comparison on a shared runner, if it is not."""
    return next((why for part, why in SHARED_RUNNER_NOISE if part in test), None)


def main(argv: list[str]) -> int:
    shared_runner = "--shared-runner" in argv
    argv = [a for a in argv if a != "--shared-runner"]
    args = [a for a in argv if a != "--tls"]
    if len(args) != (2 if "--tls" in argv else 1):
        print(__doc__, file=sys.stderr)
        return 2
    try:
        plain = load(Path(args[0]))
        describe("plain" if "--tls" in argv else "run", plain)
        if "--tls" not in argv:
            for test in sorted(t for t, o in plain.items() if o in _BAD):
                print(f"FAILED {test}")
            return 0
        tls = load(Path(args[1]))
    except Truncated as exc:
        print(f"TRUNCATED RUN: {exc}", file=sys.stderr)
        return 1
    describe("tls", tls)
    if set(tls) != set(plain):
        missing = sorted(set(plain) ^ set(tls))
        print(f"the two runs did not run the same tests; {len(missing)} differ:", file=sys.stderr)
        for test in missing[:40]:
            print(f"  {test}", file=sys.stderr)
        return 1
    for test in sorted(t for t, o in plain.items() if o in _BAD):
        print(f"FAILED in the clear{' and over TLS' if tls[test] in _BAD else ''}: {test}")
    only = tls_only_failures(plain, tls)
    held = [t for t in only if not (shared_runner and noise_reason(t))]
    for test in only:
        if test in held:
            print(f"FAILED over TLS only: {test}", file=sys.stderr)
        else:
            print(f"FAILED over TLS only, and not held to it on a shared runner: {test}")
    if held:
        return 1
    if only:
        print(f"{len(only)} failed over TLS only, each a test a shared runner fails by itself")
    else:
        print("no test fails over TLS that passes in the clear")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
