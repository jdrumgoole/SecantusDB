"""Turn PHPUnit JUnit XML into docs/validation-report-php-lib.md.

Usage:
    python -m php_lib_validation.generate_report <junit.xml> <output.md>

PHPUnit's ``--log-junit`` emits a ``<testsuites>`` tree whose ``<testcase>``
leaves carry ``file`` (absolute path) + ``class`` attributes and may contain
``<failure>`` / ``<error>`` / ``<skipped>`` children. Group by the first path
component under ``tests/`` and render the same shape of table as the pymongo /
Go / Node / Java / Ruby reports.
"""

from __future__ import annotations

import argparse
import datetime as dt
import xml.etree.ElementTree as ET
from collections import defaultdict
from pathlib import Path

import secantus


def _read_driver_version() -> str:
    head_file = Path("vendor/mongo-php-library/.git")
    try:
        if head_file.is_file():
            modules_dir = Path(head_file.read_text().strip().removeprefix("gitdir: "))
            head = (Path("vendor/mongo-php-library") / modules_dir / "HEAD").resolve()
            if head.exists():
                return head.read_text().strip()
    except Exception:
        pass
    return "unknown"


def _category_for(case: ET.Element) -> str:
    """First path component under ``tests/`` from the testcase ``file`` attr,
    falling back to the namespace tail of its ``class``."""
    file = case.attrib.get("file", "")
    marker = "/tests/"
    if marker in file:
        rel = file.split(marker, 1)[1]
        return rel.split("/", 1)[0] or "?"
    cls = case.attrib.get("class", "")
    # e.g. MongoDB\Tests\Operation\InsertOneTest -> Operation
    parts = cls.split("\\")
    if "Tests" in parts:
        i = parts.index("Tests")
        if i + 1 < len(parts):
            return parts[i + 1]
    return parts[-1] if parts else "?"


def render(xml_path: Path, out_path: Path) -> None:
    root = ET.parse(xml_path).getroot()

    by_cat: dict[str, dict[str, int]] = defaultdict(
        lambda: {"passed": 0, "failed": 0, "skipped": 0}
    )
    failures: list[tuple[str, str]] = []
    duration = 0.0

    for suite in root.iter("testsuite"):
        # Top-level testsuites carry the aggregate time; sum the outermost.
        if suite.attrib.get("time") and suite is not root:
            pass
    # Total run time = sum of top-level testsuite times (children of root).
    for child in list(root):
        try:
            duration += float(child.attrib.get("time", 0.0))
        except ValueError:
            pass

    for case in root.iter("testcase"):
        cat = _category_for(case)
        name = f"{case.attrib.get('class', '?')}::{case.attrib.get('name', '?')}"
        if case.find("failure") is not None or case.find("error") is not None:
            by_cat[cat]["failed"] += 1
            failures.append((cat, name))
        elif case.find("skipped") is not None:
            by_cat[cat]["skipped"] += 1
        else:
            by_cat[cat]["passed"] += 1

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
    md.append("# mongo-php-library Validation Report")
    md.append("")
    md.append(
        f"Generated {dt.date.today().isoformat()} — SecantusDB "
        f"{secantus.__version__} vs mongo-php-library "
        f"{_read_driver_version()[:12]} (`vendor/mongo-php-library/`)."
    )
    md.append("")
    md.append(
        "Run `uv run python -m invoke validate-php-lib` to refresh. The pass "
        "rate is the analogue of the pymongo / mongo-go-driver / "
        "mongo-node-driver / mongo-java-driver / mongo-ruby-driver gauges for "
        "the official high-level PHP library — the `mongodb/mongodb` package "
        "Laravel + Symfony applications build on."
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
        md.append("First 30 failed cases for triage:")
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
        "**mongo-php-library's PHPUnit suite is run unmodified, against a "
        "standalone SecantusDB daemon.** The submodule at "
        "`vendor/mongo-php-library/` is checked out at the pinned upstream tag "
        "with zero local edits. `php_lib_validation/runner.py` runs "
        "`composer install` (one-time per checkout) to materialise PHPUnit, "
        "boots `python -m secantus --storage-path <tempdir>`, then runs "
        "`vendor/bin/phpunit --log-junit <xml>` over the curated functional "
        "directories in `include_paths.py` with "
        "`MONGODB_URI=mongodb://127.0.0.1:<port>/` and "
        "`MONGODB_DATABASE=phplib_test` — the env vars `tests/TestCase.php` "
        "reads. The on-disk tempdir is removed after the run."
    )
    md.append("")
    md.append(
        "Every functional test opens a real TCP connection to the SecantusDB "
        "daemon and exchanges wire commands end-to-end, so the pass rate "
        "measures SecantusDB's compatibility with the PHP library, not the "
        "library's own pure-code logic. The include set is narrow on purpose: "
        "the spec-corpus suites (`SpecTests` / `UnifiedSpecTests`), GridFS, and "
        "the documentation-example tests need replica-set / transaction / CSFLE "
        "orchestration SecantusDB doesn't provide, so they're excluded rather "
        "than counted as environment-gated skips."
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
