"""Turn the pgtest gauge's ``go test -json`` stream into
docs/validation-report-pgtest.md.

Usage:
    python -m pgtest_validation.generate_report <raw.json> <output.md>

One row per corpus file (``TestPGTest/<file>`` subtests); files in
``EXPECTED_DIVERGENCES`` count as expected when they fail and are flagged
loudly when they pass.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import re
from pathlib import Path

from . import CRDB_COMMIT
from .include_paths import EXPECTED_DIVERGENCES


def _secantus_version() -> str:
    try:
        import secantus

        return secantus.__version__
    except ImportError:
        init = Path(__file__).resolve().parent.parent / "src" / "secantus" / "__init__.py"
        m = re.search(r'__version__ = "([^"]+)"', init.read_text())
        return m.group(1) if m else "unknown"


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("raw_json")
    parser.add_argument("output_md")
    args = parser.parse_args()

    results: dict[str, str] = {}
    for line in Path(args.raw_json).read_text().splitlines():
        try:
            e = json.loads(line)
        except json.JSONDecodeError:
            continue
        test = e.get("Test") or ""
        if e.get("Action") in ("pass", "fail", "skip") and test.startswith("TestPGTest/"):
            results[test.split("/", 1)[1]] = e["Action"]

    passed = sorted(f for f, a in results.items() if a == "pass")
    failed = sorted(f for f, a in results.items() if a == "fail")
    skipped = sorted(f for f, a in results.items() if a == "skip")
    expected = [f for f in failed if f in EXPECTED_DIVERGENCES]
    unexpected = [f for f in failed if f not in EXPECTED_DIVERGENCES]
    resolved = [f for f in passed if f in EXPECTED_DIVERGENCES]

    lines = [
        "# pgtest wire-protocol conformance report",
        "",
        f"- SecantusDB (Python server) {_secantus_version()}",
        f"- corpus + runner: cockroachdb/cockroach @ `{CRDB_COMMIT[:12]}` "
        "(`pkg/sql/pgwire/testdata/pgtest`, run by `pkg/testutils/pgtest` verbatim)",
        f"- generated: {dt.datetime.now(dt.timezone.utc).strftime('%Y-%m-%d %H:%M UTC')}",
        "",
        f"**{len(passed)}/{len(results)} files pass** "
        f"({len(expected)} expected divergences, {len(unexpected)} unexpected failures, "
        f"{len(skipped)} skipped).",
        "",
        "| file | result |",
        "|---|---|",
    ]
    for f in sorted(results):
        a = results[f]
        if a == "pass":
            status = "pass"
        elif a == "skip":
            status = "skip"
        elif f in EXPECTED_DIVERGENCES:
            status = "expected divergence"
        else:
            status = "**FAIL**"
        lines.append(f"| `{f}` | {status} |")

    if resolved:
        lines += ["", "## Resolved divergences — drop their entries", ""]
        lines += [f"- `{f}`" for f in resolved]
    if expected:
        lines += ["", "## Expected divergences", ""]
        lines += [f"- `{f}` — {EXPECTED_DIVERGENCES[f]}" for f in expected]
    if unexpected:
        lines += ["", "## Unexpected failures", ""]
        lines += [f"- `{f}`" for f in unexpected]

    Path(args.output_md).write_text("\n".join(lines) + "\n")
    print(
        f"pgtest gauge: {len(passed)}/{len(results)} files pass "
        f"({len(unexpected)} unexpected failures)"
    )


if __name__ == "__main__":
    main()
