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


def _driver_sync_functional_scope() -> tuple[int, int]:
    """Return ``(included, total_upstream)``: number of driver-sync
    functional test classes the gauge runs versus the total available
    in ``vendor/mongo-java-driver/driver-sync/src/test/functional/``.

    The point is to surface the denominator the bare pass-rate hides:
    a 100% on a 13-of-112 include set is a different number from a 100%
    on all 112. Counts test classes rather than test methods because the
    include set is class-granular.
    """
    from java_validation.include_modules import INCLUDE

    included = sum(len(m.test_classes) for m in INCLUDE if m.task.endswith(":driver-sync:test"))
    functional_dir = (
        Path(__file__).resolve().parent.parent
        / "vendor"
        / "mongo-java-driver"
        / "driver-sync"
        / "src"
        / "test"
        / "functional"
    )
    if not functional_dir.is_dir():
        return included, 0
    total = sum(1 for _ in functional_dir.rglob("*Test.java"))
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

    rust = "-rust-server" in out_path.name
    md: list[str] = []
    md.append(
        "# mongo-java-driver Validation Report (Rust server)"
        if rust
        else "# mongo-java-driver Validation Report"
    )
    md.append("")
    md.append(
        f"Generated {dt.date.today().isoformat()} — SecantusDB "
        f"{secantus.__version__} vs mongo-java-driver "
        f"{_read_driver_version()[:12]} (`vendor/mongo-java-driver/`)."
    )
    md.append("")
    if rust:
        md.append(
            "Run `uv run python -m invoke validate-java --server rust` to "
            "refresh. The same unmodified suite as "
            "`docs/validation-report-java.md`, pointed at the standalone "
            "**Rust server** (`secantusd-rs`) instead of the Python one — "
            "the gap between the two reports is part of the Rust server's "
            "remaining to-do list."
        )
    else:
        md.append(
            "Run `uv run python -m invoke validate-java` to refresh. The pass "
            "rate is the analogue of the pymongo / mongo-go-driver / "
            "mongo-node-driver gauges for the official Java driver — the "
            "language enterprise MongoDB consumers most often use."
        )
    md.append("")
    md.append("## Scope")
    md.append("")
    included, total_in_tree = _driver_sync_functional_scope()
    pct = (included / total_in_tree * 100) if total_in_tree else 0.0
    md.append(
        f"`driver-sync/src/test/functional/` contains **{total_in_tree}** "
        f"test classes upstream. The gauge currently runs **{included}** "
        f"of them (~{pct:.0f}%). The other {total_in_tree - included} are "
        "either intentionally out of scope (encryption / atlas-search / "
        "kotlin-or-scala wrappers / OCSP / DNS / retryable / monitoring) "
        "or unaudited — they haven't been added to "
        "`java_validation/include_modules.py` because each new class needs "
        "the runner's wall-clock guard to confirm it terminates before it "
        "ships. The pass rate below describes the included subset, "
        "not the whole functional tree."
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
    if rust:
        md.append(
            "In `--server rust` mode the same two-phase spawn runs the "
            "standalone `secantusd-rs` binary (via `gauge_common.for_server`, "
            "same flags) instead of `python -m secantus`, so the numbers "
            "above measure the Rust server."
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
