"""Walk JUnit XML output → docs/validation-report-java.md.

Usage:
    python -m java_validation.generate_report <xml-dir> <output.md>

Each XML file is one test class (`<testsuite tests=N failures=K
errors=L skipped=M>` with `<testcase>` children). Group by Gradle
module (the directory name we copied them under) and emit the same
shape of markdown table as the other gauges.
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
    md.append("# mongo-java-driver Validation Report")
    md.append("")
    md.append(
        f"Generated {dt.date.today().isoformat()} — SecantusDB "
        f"{secantus.__version__} vs mongo-java-driver "
        f"{_read_driver_version()[:12]} (`vendor/mongo-java-driver/`)."
    )
    md.append("")
    md.append(
        "Run `uv run python -m invoke validate-java` to refresh. The pass "
        "rate is the analogue of the pymongo / mongo-go-driver / "
        "mongo-node-driver gauges for the official Java driver — the "
        "language enterprise MongoDB consumers most often use."
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
        "**mongo-java-driver's tests are run unmodified, against a "
        "standalone SecantusDB daemon.** The submodule at "
        "`vendor/mongo-java-driver/` is checked out at the pinned upstream "
        "tag with zero local edits. `java_validation/runner.py` does a "
        "two-phase spawn: phase 1 boots `python -m secantus --port 27018 "
        "--storage-path <tempdir> --standalone` without `--auth` and "
        "uses pymongo to createUser `root-user` (root role); phase 2 "
        "stops that daemon and restarts on the same tempdir **with "
        "`--auth`**, so the user record persists and the server now "
        "enforces auth. Gradle then runs the driver's bundled wrapper "
        "(`./gradlew --no-daemon -Dorg.mongodb.test.uri=mongodb://"
        "root-user:password@127.0.0.1:27018/?authSource=admin`) for the "
        "in-scope modules in `java_validation/include_modules.py`. The "
        "system property is the seam Java's `ClusterFixture` test "
        "infrastructure reads; Gradle forwards it to the test JVM. "
        "Standalone topology is critical: without `--standalone` the "
        "driver's `getSecondary()` is an unbounded sleep loop on "
        "non-RS deployments."
    )
    md.append("")
    md.append(
        "These are **integration specs** under "
        "`driver-sync/src/test/functional/` — every test opens a real "
        "TCP connection to the SecantusDB daemon, SCRAM-authenticates, "
        "and exchanges wire commands end-to-end. The pass rate is "
        "therefore a true measure of SecantusDB's compatibility with "
        "the Java driver, not of the driver's own pure-code logic."
    )
    md.append("")
    md.append(
        "The include set is currently narrow on purpose — `MongoCollectionTest`, "
        "`MongoClientTest`, `ExplainTest`, `ReadConcernTest`, "
        "`MongoWriteConcernWithResponseExceptionTest` — added one at a "
        "time as each is proven to terminate against SecantusDB. The "
        "driver writes JUnit XML to "
        "`<module>/build/test-results/test/TEST-*.xml`; we copy those "
        "out of the vendored tree (so the submodule stays untouched) "
        "and parse them here. Widen `include_modules.py` to add more "
        "test classes."
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
