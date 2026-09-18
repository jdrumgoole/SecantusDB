"""Turn the psycopg gauge's pytest-json-report into docs/validation-report-psycopg.md.

Usage:
    python -m psycopg_validation.generate_report <raw.json> <output.md>

Groups tests by file (or the ``types/`` subdirectory) and emits the same
passed / failed / skipped / total / pass-rate table the driver gauges use,
plus the top failures for triage.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import re
from collections import defaultdict
from pathlib import Path

from validation_summary.expected_failures import PSYCOPG, find_match
from validation_summary.rates import pass_rate


def _secantus_version() -> str:
    """The Python-server version, without importing the WT-linked package
    (report generation must work in a bare worktree venv)."""
    try:
        import secantus

        return secantus.__version__
    except ImportError:
        init = Path(__file__).resolve().parent.parent / "src" / "secantus" / "__init__.py"
        m = re.search(r'__version__ = "([^"]+)"', init.read_text())
        return m.group(1) if m else "unknown"


def _category_for(nodeid: str) -> str:
    rel = nodeid
    if rel.startswith("tests/"):
        rel = rel[len("tests/") :]
    head = rel.split("::", 1)[0]
    parts = head.split("/")
    # types/test_numeric.py stays per-file (the type suites are the meat);
    # everything else is already a file.
    return "/".join(parts) if len(parts) <= 2 else parts[0]


def _read_psycopg_version() -> str:
    """Best-effort read of the pinned psycopg tag from the submodule."""
    head_file = Path("vendor/psycopg/.git")
    try:
        if head_file.is_file():
            gitdir = head_file.read_text().split(": ", 1)[1].strip()
            head = (Path(gitdir) / "HEAD").read_text().strip()
        else:
            head = (head_file / "HEAD").read_text().strip()
        return head[:12]
    except OSError:
        return "unknown"


_OUTCOME_BUCKETS = {
    "passed": "passed",
    "subtests passed": "passed",
    "failed": "failed",
    "error": "failed",
    "subtests failed": "failed",
    "skipped": "skipped",
    "xfailed": "skipped",
    "xpassed": "passed",
    "deselected": "skipped",
}


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("raw_json")
    parser.add_argument("output_md")
    args = parser.parse_args()

    data = json.loads(Path(args.raw_json).read_text())
    per_cat: dict[str, dict[str, int]] = defaultdict(
        lambda: {"passed": 0, "failed": 0, "expected": 0, "skipped": 0}
    )
    failures: list[str] = []
    expected: list[tuple[str, str]] = []
    for t in data.get("tests", []):
        bucket = _OUTCOME_BUCKETS.get(t.get("outcome", ""), "failed")
        nodeid = t["nodeid"]
        if bucket == "failed":
            # A documented non-server failure counts separately, so the pass
            # rate is neither gamed by dropping it nor dragged down by a test
            # whose outcome this server cannot influence. Both columns ship.
            match = find_match(PSYCOPG, nodeid)
            if match is not None:
                bucket = "expected"
                expected.append((nodeid, match.rationale))
            else:
                failures.append(nodeid)
        per_cat[_category_for(nodeid)][bucket] += 1

    totals = {"passed": 0, "failed": 0, "expected": 0, "skipped": 0}
    lines = [
        "# psycopg conformance report",
        "",
        f"- SecantusDB (Python server) {_secantus_version()}",
        f"- psycopg suite: vendor/psycopg @ {_read_psycopg_version()}",
        f"- generated: {dt.datetime.now(dt.timezone.utc).strftime('%Y-%m-%d %H:%M UTC')}",
        "",
        "| category | passed | failed | expected | skipped | total | pass rate | adjusted |",
        "|---|---|---|---|---|---|---|---|",
    ]
    for cat in sorted(per_cat):
        c = per_cat[cat]
        for k in totals:
            totals[k] += c[k]
        run = c["passed"] + c["failed"] + c["expected"]
        rate = pass_rate(c["passed"], run)
        adj = pass_rate(c["passed"], c["passed"] + c["failed"])
        lines.append(
            f"| {cat} | {c['passed']} | {c['failed']} | {c['expected']} | {c['skipped']} "
            f"| {sum(c.values())} | {rate} | {adj} |"
        )
    run = totals["passed"] + totals["failed"] + totals["expected"]
    rate = pass_rate(totals["passed"], run)
    adj = pass_rate(totals["passed"], totals["passed"] + totals["failed"])
    lines.append(
        f"| **total** | **{totals['passed']}** | **{totals['failed']}** "
        f"| **{totals['expected']}** | **{totals['skipped']}** "
        f"| **{sum(totals.values())}** | **{rate}** | **{adj}** |"
    )

    if expected:
        lines += [
            "",
            f"## Expected failures ({len(expected)})",
            "",
            "Documented non-server failures. They are excluded from the *adjusted* "
            "rate and counted in the plain one, so neither number hides them. Each "
            "entry's rationale lives in `validation_summary/expected_failures.py`.",
            "",
        ]
        lines += [f"- `{n}` — {r}" for n, r in sorted(expected)]

    if failures:
        lines += ["", f"## Failures ({len(failures)})", ""]
        lines += [f"- `{f}`" for f in sorted(failures)]

    Path(args.output_md).write_text("\n".join(lines) + "\n")
    print(
        f"psycopg gauge: {totals['passed']} passed / {totals['failed']} failed "
        f"/ {totals['expected']} expected / {totals['skipped']} skipped "
        f"({rate}, adjusted {adj})"
    )


if __name__ == "__main__":
    main()
