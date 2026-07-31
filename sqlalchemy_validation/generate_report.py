"""Turn the SQLAlchemy gauge's pytest-json-report into
docs/validation-report-sqlalchemy.md.

Usage:
    python -m sqlalchemy_validation.generate_report <raw.json> <output.md>

Groups tests by compliance-suite class (the ``_postgresql+psycopg_…`` dialect
suffix stripped) and emits the same passed / failed / skipped / total /
pass-rate table the other gauges use, plus the failures for triage.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import re
from collections import defaultdict
from pathlib import Path


def _secantus_version() -> str:
    try:
        import secantus

        return secantus.__version__
    except ImportError:
        init = Path(__file__).resolve().parent.parent / "src" / "secantus" / "__init__.py"
        m = re.search(r'__version__ = "([^"]+)"', init.read_text())
        return m.group(1) if m else "unknown"


def _sqlalchemy_version() -> str:
    try:
        import sqlalchemy

        return sqlalchemy.__version__
    except ImportError:
        return "unknown"


_DIALECT_SUFFIX = re.compile(r"_postgresql\+\w+_[\d_]+$")


def _category_for(nodeid: str) -> str:
    parts = nodeid.split("::")
    cls = parts[1] if len(parts) > 1 else parts[0]
    return _DIALECT_SUFFIX.sub("", cls)


_OUTCOME_BUCKETS = {
    "passed": "passed",
    "failed": "failed",
    "error": "failed",
    "skipped": "skipped",
    "xfailed": "skipped",
    "xpassed": "passed",
}


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("raw_json")
    parser.add_argument("output_md")
    args = parser.parse_args()

    data = json.loads(Path(args.raw_json).read_text())
    per_cat: dict[str, dict[str, int]] = defaultdict(
        lambda: {"passed": 0, "failed": 0, "skipped": 0}
    )
    failures: list[str] = []
    for t in data.get("tests", []):
        bucket = _OUTCOME_BUCKETS.get(t.get("outcome", ""), "failed")
        per_cat[_category_for(t["nodeid"])][bucket] += 1
        if bucket == "failed":
            failures.append(t["nodeid"])

    totals = {"passed": 0, "failed": 0, "skipped": 0}
    lines = [
        "# SQLAlchemy dialect-compliance report",
        "",
        f"- SecantusDB (Python server) {_secantus_version()}",
        f"- suite: sqlalchemy.testing.suite @ SQLAlchemy {_sqlalchemy_version()}, "
        "postgresql+psycopg dialect",
        f"- generated: {dt.datetime.now(dt.timezone.utc).strftime('%Y-%m-%d %H:%M UTC')}",
        "",
        "| suite class | passed | failed | skipped | total | pass rate |",
        "|---|---|---|---|---|---|",
    ]
    for cat in sorted(per_cat):
        c = per_cat[cat]
        for k in totals:
            totals[k] += c[k]
        run = c["passed"] + c["failed"]
        rate = f"{c['passed'] / run * 100:.1f}%" if run else "—"
        lines.append(
            f"| {cat} | {c['passed']} | {c['failed']} | {c['skipped']} "
            f"| {sum(c.values())} | {rate} |"
        )
    run = totals["passed"] + totals["failed"]
    rate = f"{totals['passed'] / run * 100:.1f}%" if run else "—"
    lines.append(
        f"| **total** | **{totals['passed']}** | **{totals['failed']}** "
        f"| **{totals['skipped']}** | **{sum(totals.values())}** | **{rate}** |"
    )

    if failures:
        lines += ["", f"## Failures ({len(failures)})", ""]
        lines += [f"- `{f}`" for f in sorted(failures)]

    Path(args.output_md).write_text("\n".join(lines) + "\n")
    print(
        f"sqlalchemy gauge: {totals['passed']} passed / {totals['failed']} failed "
        f"/ {totals['skipped']} skipped ({rate})"
    )


if __name__ == "__main__":
    main()
