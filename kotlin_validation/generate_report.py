"""Walk JUnit XML output → docs/validation-report-kotlin.md.

Usage:
    python -m kotlin_validation.generate_report <xml-dir> <output.md>

Each XML file is one test class (``<testsuite tests=N failures=K errors=L
skipped=M>`` with ``<testcase>`` children). Group by Gradle module (the
directory name we copied them under) and emit the same shape of markdown
table as the other gauges — the Kotlin analogue of
``java_validation/generate_report.py``.
"""

from __future__ import annotations

import argparse
import datetime as dt
import xml.etree.ElementTree as ET
from collections import defaultdict
from pathlib import Path

import secantus


def _read_driver_version() -> str:
    head_file = Path("vendor/mongo-java-driver/.git")
    try:
        if head_file.is_file():
            modules_dir = Path(head_file.read_text().strip().removeprefix("gitdir: "))
            head = (Path("vendor/mongo-java-driver") / modules_dir / "HEAD").resolve()
            if head.exists():
                return head.read_text().strip()
    except Exception:
        pass
    return "unknown"


def _kotlin_integration_scope() -> tuple[int, int]:
    """Return ``(included, total_upstream)``: number of Kotlin sync
    integration test classes the gauge runs versus the total available in
    ``driver-kotlin-sync/src/integrationTest/``.

    Surfaces the denominator the bare pass-rate hides. Counts ``*Test*.kt``
    classes (excluding the ``syncadapter/`` shim package, which is plumbing,
    not tests).
    """
    from kotlin_validation.include_modules import INCLUDE

    included = sum(len(m.test_classes) for m in INCLUDE)
    integ_dir = (
        Path(__file__).resolve().parent.parent
        / "vendor"
        / "mongo-java-driver"
        / "driver-kotlin-sync"
        / "src"
        / "integrationTest"
    )
    if not integ_dir.is_dir():
        return included, 0
    total = sum(
        1
        for p in integ_dir.rglob("*.kt")
        if "syncadapter" not in p.parts and ("Test" in p.stem or "Smoke" in p.stem)
    )
    return included, total


def render(xml_dir: Path, out_path: Path) -> None:
    by_mod: dict[str, dict[str, int]] = defaultdict(
        lambda: {"passed": 0, "failed": 0, "skipped": 0}
    )
    failures: list[tuple[str, str]] = []  # (module, classname#testname)

    for xml in xml_dir.rglob("TEST-*.xml"):
        module = xml.parent.name
        try:
            root = ET.parse(xml).getroot()
        except ET.ParseError:
            continue
        for case in root.iter("testcase"):
            classname = case.attrib.get("classname", "?")
            name = case.attrib.get("name", "?")
            if case.find("failure") is not None or case.find("error") is not None:
                by_mod[module]["failed"] += 1
                failures.append((module, f"{classname}#{name}"))
            elif case.find("skipped") is not None:
                by_mod[module]["skipped"] += 1
            else:
                by_mod[module]["passed"] += 1

    rows: list[tuple[str, int, int, int, int, str]] = []
    totals = {"passed": 0, "failed": 0, "skipped": 0}
    for mod in sorted(by_mod):
        b = by_mod[mod]
        total = b["passed"] + b["failed"] + b["skipped"]
        ran = b["passed"] + b["failed"]
        rate = f"{(b['passed'] / ran * 100):.1f}%" if ran else "—"
        rows.append((mod, b["passed"], b["failed"], b["skipped"], total, rate))
        for k in totals:
            totals[k] += b[k]

    grand_total = sum(totals.values())
    grand_ran = totals["passed"] + totals["failed"]
    grand_rate = f"{(totals['passed'] / grand_ran * 100):.1f}%" if grand_ran else "—"

    md: list[str] = []
    md.append("# mongo-kotlin-driver Validation Report")
    md.append("")
    md.append(
        f"Generated {dt.date.today().isoformat()} — SecantusDB "
        f"{secantus.__version__} vs mongo-kotlin-driver "
        f"{_read_driver_version()[:12]} (`vendor/mongo-java-driver/driver-kotlin-sync/`)."
    )
    md.append("")
    md.append(
        "Run `uv run python -m invoke validate-kotlin` to refresh. This is the "
        "official MongoDB **Kotlin** driver gauge — the Kotlin sync client (which "
        "ships in the mongo-java-driver monorepo) exercised end-to-end against a "
        "standalone SecantusDB daemon."
    )
    md.append("")
    md.append("## Scope")
    md.append("")
    included, total_in_tree = _kotlin_integration_scope()
    pct = (included / total_in_tree * 100) if total_in_tree else 0.0
    md.append(
        f"`driver-kotlin-sync/src/integrationTest/` contains **{total_in_tree}** "
        f"test classes upstream. The gauge currently runs **{included}** of them "
        f"(~{pct:.0f}%). The rest are either out of scope (search-index / "
        "change-stream-resume / retryable scenarios that need features SecantusDB "
        "doesn't implement) or unaudited — each new class needs the runner's "
        "wall-clock guard to confirm it terminates before it's added to "
        "`kotlin_validation/include_modules.py`. The pass rate below describes the "
        "included subset, not the whole integration tree."
    )
    md.append("")
    md.append("## Summary by module")
    md.append("")
    md.append("| Module | Passed | Failed | Skipped | Total | Pass rate |")
    md.append("|---|---:|---:|---:|---:|---:|")
    for mod, p, f, s, t, r in rows:
        md.append(f"| `{mod}` | {p} | {f} | {s} | {t} | {r} |")
    md.append(
        f"| **Overall** | **{totals['passed']}** | **{totals['failed']}** | "
        f"**{totals['skipped']}** | **{grand_total}** | **{grand_rate}** |"
    )
    md.append("")

    if failures:
        md.append(f"## Failures ({len(failures)})")
        md.append("")
        md.append("First 30 failed tests for triage:")
        md.append("")
        md.append("```")
        for mod, ref in failures[:30]:
            md.append(f"{mod} :: {ref}")
        md.append("```")
        if len(failures) > 30:
            md.append(f"... and {len(failures) - 30} more (see raw XML).")
        md.append("")

    md.append("## How this is generated")
    md.append("")
    md.append(
        "**mongo-kotlin-driver's integration tests are run unmodified, against a "
        "standalone SecantusDB daemon.** The Kotlin driver lives in the "
        "mongo-java-driver monorepo (`vendor/mongo-java-driver/driver-kotlin-sync/`), "
        "checked out at the pinned upstream tag with zero local edits. "
        "`kotlin_validation/runner.py` does the same two-phase spawn as the Java "
        "gauge: phase 1 boots `python -m secantus --port 0 --storage-path <tempdir> "
        "--standalone` without `--auth` and seeds `root-user` (root role) via "
        "pymongo; phase 2 restarts on the same tempdir **with `--auth`**. Gradle "
        "then runs the bundled wrapper's `:driver-kotlin-sync:integrationTest` task "
        "with `-Dorg.mongodb.test.uri=mongodb://root-user:password@…/?authSource=admin`."
    )
    md.append("")
    md.append(
        "These are **integration tests** under "
        "`driver-kotlin-sync/src/integrationTest/` — every test opens a real TCP "
        "connection through the Kotlin client (over its `syncadapter` shim), "
        "SCRAM-authenticates, and exchanges wire commands. The pure-Mockito unit "
        "tests under `src/test/` never touch a server and are intentionally "
        "excluded. The driver writes JUnit XML to "
        "`<module>/build/test-results/integrationTest/TEST-*.xml`; we copy those "
        "out of the vendored tree (so the submodule stays untouched) and parse "
        "them here. Widen `kotlin_validation/include_modules.py` to add more classes."
    )
    md.append("")

    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text("\n".join(md))


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("xml_dir", type=Path)
    parser.add_argument("output_md", type=Path)
    args = parser.parse_args()
    render(args.xml_dir, args.output_md)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
