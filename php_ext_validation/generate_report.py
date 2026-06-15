"""Turn run-tests.php JUnit XML into docs/validation-report-php-ext.md.

Usage:
    python -m php_ext_validation.generate_report <junit.xml> <output.md>

PHP's ``run-tests.php`` emits a nested ``<testsuites>`` tree: one
``<testsuite name="tests/<dir>">`` per directory, each with ``<testcase>``
leaves. A skipped ``.phpt`` shows up as a testcase with an ``<error>`` /
``<skipped>`` marker; a real failure has ``<failure>`` (or a non-skip
``<error>``). Group by the directory testsuite name and render the same shape
of table as the other gauges.
"""

from __future__ import annotations

import argparse
import datetime as dt
import xml.etree.ElementTree as ET
from collections import defaultdict
from pathlib import Path

import secantus


def _read_driver_version() -> str:
    head_file = Path("vendor/mongo-php-driver/.git")
    try:
        if head_file.is_file():
            modules_dir = Path(head_file.read_text().strip().removeprefix("gitdir: "))
            head = (Path("vendor/mongo-php-driver") / modules_dir / "HEAD").resolve()
            if head.exists():
                return head.read_text().strip()
    except Exception:
        pass
    return "unknown"


def _category(case_name: str) -> str:
    """Derive the test directory from a run-tests.php testcase name.

    Names look like ``tests/bson/bson-binary-001.phpt (description)`` — take
    the path component right after ``tests/``. Falls back to ``?``.
    """
    n = case_name or ""
    marker = "tests/"
    if marker in n:
        rel = n.split(marker, 1)[1]
        return rel.split("/", 1)[0] or "?"
    return "?"


def _classify(case: ET.Element) -> str:
    """Return 'passed' | 'failed' | 'skipped' for a testcase element."""
    if case.find("failure") is not None:
        return "failed"
    err = case.find("error")
    if err is not None:
        kind = (err.attrib.get("type", "") + " " + (err.text or "")).upper()
        return "skipped" if "SKIP" in kind else "failed"
    if case.find("skipped") is not None:
        return "skipped"
    return "passed"


def render(xml_path: Path, out_path: Path) -> None:
    root = ET.parse(xml_path).getroot()

    by_cat: dict[str, dict[str, int]] = defaultdict(
        lambda: {"passed": 0, "failed": 0, "skipped": 0}
    )
    failures: list[tuple[str, str]] = []

    # run-tests.php emits <testsuites time="..."> with the aggregate run time.
    try:
        duration = float(root.attrib.get("time", 0.0))
    except ValueError:
        duration = 0.0

    for case in root.iter("testcase"):
        name = case.attrib.get("name", "?")
        cat = _category(name)
        status = _classify(case)
        by_cat[cat][status] += 1
        if status == "failed":
            failures.append((cat, name))

    rows: list[tuple[str, int, int, int, int, str]] = []
    totals = {"passed": 0, "failed": 0, "skipped": 0}
    for cat in sorted(by_cat):
        b = by_cat[cat]
        total = b["passed"] + b["failed"] + b["skipped"]
        ran = b["passed"] + b["failed"]
        rate = f"{(b['passed'] / ran * 100):.1f}%" if ran else "—"
        rows.append((cat, b["passed"], b["failed"], b["skipped"], total, rate))
        for k in totals:
            totals[k] += b[k]

    grand_total = sum(totals.values())
    grand_ran = totals["passed"] + totals["failed"]
    grand_rate = f"{(totals['passed'] / grand_ran * 100):.1f}%" if grand_ran else "—"

    md: list[str] = []
    md.append("# mongo-php-driver Validation Report")
    md.append("")
    md.append(
        f"Generated {dt.date.today().isoformat()} — SecantusDB "
        f"{secantus.__version__} vs mongo-php-driver "
        f"{_read_driver_version()[:12]} (`vendor/mongo-php-driver/`)."
    )
    md.append("")
    md.append(
        "Run `uv run python -m invoke validate-php-ext` to refresh. This is the "
        "low-level PHP extension (the PECL `mongodb` package that wraps "
        "libmongoc) — the strictest wire-protocol gauge, alongside "
        "mongo-go-driver, for catching bugs pymongo's permissive client misses."
    )
    md.append("")
    md.append("## Summary by category")
    md.append("")
    md.append("| Category | Passed | Failed | Skipped | Total | Pass rate |")
    md.append("|---|---:|---:|---:|---:|---:|")
    for cat, p, f, sk, t, r in rows:
        md.append(f"| `tests/{cat}` | {p} | {f} | {sk} | {t} | {r} |")
    md.append(
        f"| **Overall** | **{totals['passed']}** | **{totals['failed']}** | "
        f"**{totals['skipped']}** | **{grand_total}** | **{grand_rate}** |"
    )
    md.append("")
    md.append(f"Run time: {duration:.2f}s.")
    md.append("")

    if failures:
        md.append(f"## Failures ({len(failures)})")
        md.append("")
        md.append("First 30 failed tests for triage:")
        md.append("")
        md.append("```")
        for cat, title in failures[:30]:
            md.append(f"tests/{cat} :: {title}")
        md.append("```")
        if len(failures) > 30:
            md.append(f"... and {len(failures) - 30} more (see JUnit XML).")
        md.append("")

    md.append("## How this is generated")
    md.append("")
    md.append(
        "**mongo-php-driver's `.phpt` suite is run unmodified, against a "
        "standalone SecantusDB daemon.** The submodule at "
        "`vendor/mongo-php-driver/` is checked out at the pinned upstream tag "
        "(matching the installed extension version) with zero local edits. "
        "`php_ext_validation/runner.py` boots `python -m secantus "
        "--storage-path <tempdir>`, then runs PHP's `run-tests.php` over the "
        "curated directories in `include_paths.py` against the "
        "**already-installed** `mongodb` extension — no rebuild — with "
        "`MONGODB_URI=mongodb://127.0.0.1:<port>/` (the var "
        "`tests/utils/basic.inc` reads) and `TEST_PHP_JUNIT` pointing at the "
        "JUnit output. The on-disk tempdir is removed after the run."
    )
    md.append("")
    md.append(
        "The `tests/bson` directory is pure-driver BSON serialization (no "
        "server); every other included directory opens a real TCP connection "
        "and exchanges wire commands end-to-end. The `.phpt` files self-guard "
        "by topology via `skip_if_*` helpers, so tests that need a replica set, "
        "transactions, or CSFLE SKIP cleanly. The include set excludes the "
        "huge `bson-corpus` directory and the orchestration-dependent suites "
        "(`session`, `retryable-*`, `replicaset`, `clientEncryption`) rather "
        "than counting them as skips."
    )
    md.append("")

    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text("\n".join(md))


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("junit_xml", type=Path)
    parser.add_argument("output_md", type=Path)
    args = parser.parse_args()
    render(args.junit_xml, args.output_md)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
