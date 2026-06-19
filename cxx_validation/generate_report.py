"""Render mongocxx's Catch2 JUnit XML into ``docs/validation-report-cxx.md``.

Same shape as the other gauges' report generators: a per-group table of
passed / failed / skipped counts, an overall row, and a truncated failures
list for triage.

Catch2's JUnit reporter emits ``<testsuites><testsuite name="..."><testcase
classname="..." name="..."><failure/>|<skipped/></testcase>``. We group by the
testcase ``classname`` (Catch2 sets it to the test's source context).

Usage::

    uv run python -m cxx_validation.generate_report \\
        .validation/cxx-raw.xml docs/validation-report-cxx.md
"""

from __future__ import annotations

import datetime as _dt
import subprocess
import sys
import xml.etree.ElementTree as ET
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
VENDOR = REPO_ROOT / "vendor" / "mongo-cxx-driver"


def _vendor_ref() -> str:
    try:
        out = subprocess.run(
            ["git", "-C", str(VENDOR), "describe", "--tags", "--always"],
            capture_output=True,
            text=True,
            check=True,
        )
        return out.stdout.strip()
    except (subprocess.SubprocessError, FileNotFoundError):
        return "detached"


def _secantus_version() -> str:
    init = REPO_ROOT / "src" / "secantus" / "__init__.py"
    for line in init.read_text().splitlines():
        if line.startswith("__version__"):
            return line.split("=", 1)[1].strip().strip('"').strip("'")
    return "unknown"


def _classify(case: ET.Element) -> str:
    if case.find("failure") is not None or case.find("error") is not None:
        return "failed"
    if case.find("skipped") is not None:
        return "skipped"
    return "passed"


def render(xml_path: Path, out_path: Path) -> None:
    root = ET.parse(xml_path).getroot()

    # Catch2's JUnit output carries no usable per-category signal (``classname``
    # is a uniform ``<binary>.global``; tags don't survive into JUnit), so a
    # per-test-name table would be one row per TEST_CASE — hundreds of rows. We
    # report the overall total plus the full failures list (the actionable
    # part); the cross-driver summary handles like-for-like comparison.
    totals = {"passed": 0, "failed": 0, "skipped": 0}
    failures: list[str] = []
    for case in root.iter("testcase"):
        status = _classify(case)
        totals[status] += 1
        if status == "failed":
            failures.append(case.attrib.get("name", "?"))

    grand_total = sum(totals.values())
    grand_ran = totals["passed"] + totals["failed"]
    grand_rate = f"{(totals['passed'] / grand_ran * 100):.1f}%" if grand_ran else "—"

    md: list[str] = []
    md.append("# mongo-cxx-driver Validation Report")
    md.append("")
    md.append(
        f"Generated {_dt.date.today().isoformat()} — "
        f"SecantusDB {_secantus_version()} vs mongo-cxx-driver "
        f"{_vendor_ref()} (`vendor/mongo-cxx-driver/`)."
    )
    md.append("")
    md.append(
        "Run `uv run python -m invoke validate-cxx` to refresh. The official "
        "MongoDB **C++** driver (`mongocxx`), built on libmongoc — its Catch2 "
        "`test_driver` suite (CRUD / cursor / aggregate / GridFS / commands) "
        "run unmodified against an embedded SecantusDB daemon."
    )
    md.append("")
    md.append("## Summary")
    md.append("")
    md.append("| Passed | Failed | Skipped | Total | Pass rate |")
    md.append("|---:|---:|---:|---:|---:|")
    md.append(
        f"| {totals['passed']} | {totals['failed']} | {totals['skipped']} | "
        f"{grand_total} | {grand_rate} |"
    )
    md.append("")
    md.append(
        "(Catch2 expands each `SECTION` into its own JUnit `<testcase>`, so the "
        "total exceeds the number of `TEST_CASE`s.)"
    )
    md.append("")
    if failures:
        md.append(f"## Failures ({len(failures)})")
        md.append("")
        md.append("First 30 failed tests for triage:")
        md.append("")
        md.append("```")
        for f in failures[:30]:
            md.append(f)
        md.append("```")
        if len(failures) > 30:
            md.append(f"... and {len(failures) - 30} more (see the JUnit XML).")
        md.append("")
    md.append("## How this is generated")
    md.append("")
    md.append(
        "`invoke validate-cxx` builds the vendored libmongoc (installed to a "
        "prefix) and the mongocxx `test_driver` Catch2 binary against it, then "
        "binds a SecantusDB daemon on `127.0.0.1:27017` (mongocxx's core tests "
        "hard-wire the driver default port — there's no `MONGOC_TEST_URI`-style "
        "override) and runs the suite with Catch2's JUnit reporter. Out-of-scope "
        "tags (CSFLE, Atlas, transactions, sessions, SDAM monitoring) are "
        "excluded in `cxx_validation/include_paths.py`."
    )
    md.append("")
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text("\n".join(md))


def main(argv: list[str]) -> int:
    if len(argv) != 3:
        print("Usage: generate_report.py <junit.xml> <out.md>", file=sys.stderr)
        return 2
    render(Path(argv[1]), Path(argv[2]))
    print(f"Wrote {argv[2]}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
