"""Turn the pgx gauge's ``go test -json`` stream into
docs/validation-report-pgx.md.

Usage:
    python -m pgx_validation.generate_report <raw.json> <output.md>

Counts leaf test results (subtests included) per package and lists the
failures for triage — the same passed / failed / skipped / pass-rate table
the other gauges emit.
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


def _pgx_version() -> str:
    head_file = Path("vendor/pgx/.git")
    try:
        if head_file.is_file():
            gitdir = head_file.read_text().split(": ", 1)[1].strip()
            gdir = Path(gitdir) if Path(gitdir).is_absolute() else head_file.parent / gitdir
            return (gdir / "HEAD").read_text().strip()[:12]
        return (head_file / "HEAD").read_text().strip()[:12]
    except OSError:
        return "unknown"


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("raw_json")
    parser.add_argument("output_md")
    args = parser.parse_args()

    per_pkg: dict[str, dict[str, int]] = defaultdict(
        lambda: {"passed": 0, "failed": 0, "skipped": 0}
    )
    failures: list[str] = []
    bucket = {"pass": "passed", "fail": "failed", "skip": "skipped"}
    for line in Path(args.raw_json).read_text().splitlines():
        try:
            e = json.loads(line)
        except json.JSONDecodeError:
            continue
        action = e.get("Action")
        test = e.get("Test")
        if not test or action not in bucket:
            continue
        pkg = (e.get("Package") or "?").rsplit("/", 1)[-1]
        per_pkg[pkg][bucket[action]] += 1
        if action == "fail":
            failures.append(f"{pkg} :: {test}")

    totals = {"passed": 0, "failed": 0, "skipped": 0}
    lines = [
        "# pgx (pgconn + pgproto3) conformance report",
        "",
        f"- SecantusDB (Python server) {_secantus_version()}",
        f"- suite: vendor/pgx @ {_pgx_version()} (`go test`, unmodified)",
        f"- generated: {dt.datetime.now(dt.timezone.utc).strftime('%Y-%m-%d %H:%M UTC')}",
        "",
        "| package | passed | failed | skipped | total | pass rate |",
        "|---|---|---|---|---|---|",
    ]
    for pkg in sorted(per_pkg):
        c = per_pkg[pkg]
        for k in totals:
            totals[k] += c[k]
        run = c["passed"] + c["failed"]
        rate = f"{c['passed'] / run * 100:.1f}%" if run else "—"
        lines.append(
            f"| {pkg} | {c['passed']} | {c['failed']} | {c['skipped']} "
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
        f"pgx gauge: {totals['passed']} passed / {totals['failed']} failed "
        f"/ {totals['skipped']} skipped ({rate})"
    )


if __name__ == "__main__":
    main()
