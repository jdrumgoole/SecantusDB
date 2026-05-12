"""Cross-driver validation summary.

Reads the raw output each per-driver gauge writes to ``.validation/``
and emits ``docs/validation-summary.md``: a single table comparing
pymongo / mongo-go-driver / mongo-java-driver / mongo-node-driver /
mongo-ruby-driver on the same axes (tests run, passed, failed,
skipped, pass rate). The point is like-for-like comparability —
each per-gauge report shows its own per-category breakdown in its
own native column shape, and the bare pass-rate from those isn't
directly comparable because the denominators come from different
metrics (test methods vs files vs RSpec examples vs Mocha tests).
This summary normalizes on ``test count``: every row counts an
individual assertion outcome.

Usage::

    uv run python -m validation_summary.generate \
        --raw-dir .validation --out docs/validation-summary.md
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import xml.etree.ElementTree as ET
from dataclasses import dataclass
from pathlib import Path

import secantus

REPO_ROOT = Path(__file__).resolve().parent.parent


@dataclass(frozen=True)
class GaugeStats:
    """Normalized per-gauge stats. Every gauge contributes one of these."""

    name: str  # display name, e.g. "mongo-java-driver"
    language: str  # display label, e.g. "Java"
    driver_version: str  # the upstream tag / SHA being tested against
    passed: int
    failed: int
    skipped: int
    note: str = ""  # optional scope note (e.g. "21 of 112 functional classes")

    @property
    def total(self) -> int:
        return self.passed + self.failed + self.skipped

    @property
    def ran(self) -> int:
        return self.passed + self.failed

    @property
    def pass_rate(self) -> str:
        return f"{(self.passed / self.ran * 100):.1f}%" if self.ran else "—"


def _read_submodule_head(rel: str) -> str:
    """Resolve the HEAD SHA of a vendored submodule (truncated).

    Follows ``HEAD`` even when it's a symbolic ref into ``refs/heads/``
    so that submodules pinned to a branch tip (e.g. ``ref: refs/heads/
    main``) still report a SHA rather than the literal ref string.
    """
    head_file = REPO_ROOT / "vendor" / rel / ".git"
    try:
        if not head_file.is_file():
            return "unknown"
        gitdir_str = head_file.read_text().strip().removeprefix("gitdir: ")
        # The ``gitdir`` line is relative to the submodule directory.
        gitdir = (REPO_ROOT / "vendor" / rel / gitdir_str).resolve()
        head = (gitdir / "HEAD").resolve()
        if not head.exists():
            return "unknown"
        raw = head.read_text().strip()
        if raw.startswith("ref: "):
            ref = raw.removeprefix("ref: ")
            ref_path = (gitdir / ref).resolve()
            if ref_path.exists():
                return ref_path.read_text().strip()[:12]
            packed = gitdir / "packed-refs"
            if packed.exists():
                for line in packed.read_text().splitlines():
                    if line.endswith(f" {ref}"):
                        return line.split()[0][:12]
            return "unknown"
        return raw[:12]
    except Exception:
        return "unknown"


def _collect_pymongo(raw_dir: Path) -> GaugeStats | None:
    """Read ``pytest-json-report`` output (``.validation/raw.json``)."""
    f = raw_dir / "raw.json"
    if not f.exists():
        return None
    raw = json.loads(f.read_text())
    s = raw.get("summary", {})
    passed = s.get("passed", 0) + s.get("subtests passed", 0)
    return GaugeStats(
        name="pymongo",
        language="Python",
        driver_version=_read_submodule_head("pymongo-tests"),
        passed=passed,
        failed=s.get("failed", 0) + s.get("error", 0),
        skipped=s.get("skipped", 0),
        note="curated pytest paths under vendor/pymongo-tests/test/",
    )


def _collect_go(raw_dir: Path) -> GaugeStats | None:
    """Read ``go test -json`` NDJSON (``.validation/go-raw.ndjson``)."""
    f = raw_dir / "go-raw.ndjson"
    if not f.exists():
        return None
    passed = failed = skipped = 0
    for line in f.read_text().splitlines():
        try:
            ev = json.loads(line)
        except json.JSONDecodeError:
            continue
        if not ev.get("Test"):
            continue
        action = ev.get("Action")
        if action == "pass":
            passed += 1
        elif action == "fail":
            failed += 1
        elif action == "skip":
            skipped += 1
    return GaugeStats(
        name="mongo-go-driver",
        language="Go",
        driver_version=_read_submodule_head("mongo-go-driver"),
        passed=passed,
        failed=failed,
        skipped=skipped,
        note="vendor/mongo-go-driver/internal/integration/...",
    )


def _collect_node(raw_dir: Path) -> GaugeStats | None:
    """Read Mocha JSON reporter output (``.validation/node-raw.json``)."""
    f = raw_dir / "node-raw.json"
    if not f.exists():
        return None
    raw = json.loads(f.read_text())
    s = raw.get("stats", {})
    return GaugeStats(
        name="mongo-node-driver",
        language="Node.js",
        driver_version=_read_submodule_head("node-mongodb-native"),
        passed=s.get("passes", 0),
        failed=s.get("failures", 0),
        skipped=s.get("pending", 0),
        note="curated test/integration/ spec set",
    )


def _collect_ruby(raw_dir: Path) -> GaugeStats | None:
    """Read RSpec JSON formatter output (``.validation/ruby-raw.json``)."""
    f = raw_dir / "ruby-raw.json"
    if not f.exists():
        return None
    raw = json.loads(f.read_text())
    s = raw.get("summary", {})
    failed = s.get("failure_count", 0)
    skipped = s.get("pending_count", 0)
    passed = s.get("example_count", 0) - failed - skipped
    return GaugeStats(
        name="mongo-ruby-driver",
        language="Ruby",
        driver_version=_read_submodule_head("mongo-ruby-driver"),
        passed=passed,
        failed=failed,
        skipped=skipped,
        note="curated spec/mongo/*.rb spec files",
    )


def _collect_java(raw_dir: Path) -> GaugeStats | None:
    """Walk JUnit XML output (``.validation/java-results/<module>/TEST-*.xml``)."""
    xml_dir = raw_dir / "java-results"
    if not xml_dir.is_dir():
        return None
    passed = failed = skipped = 0
    for xml in xml_dir.rglob("TEST-*.xml"):
        try:
            root = ET.parse(xml).getroot()
        except ET.ParseError:
            continue
        for case in root.iter("testcase"):
            if case.find("failure") is not None or case.find("error") is not None:
                failed += 1
            elif case.find("skipped") is not None:
                skipped += 1
            else:
                passed += 1
    # Compute the driver-sync class-coverage ratio so the note line can
    # carry the same denominator the per-gauge report surfaces.
    try:
        from java_validation.include_modules import INCLUDE

        included = sum(len(m.test_classes) for m in INCLUDE if m.task.endswith(":driver-sync:test"))
        functional_dir = (
            REPO_ROOT
            / "vendor"
            / "mongo-java-driver"
            / "driver-sync"
            / "src"
            / "test"
            / "functional"
        )
        total_classes = (
            sum(1 for _ in functional_dir.rglob("*Test.java")) if functional_dir.is_dir() else 0
        )
        note = (
            f"{included} of {total_classes} driver-sync functional classes + bson unit tests"
            if total_classes
            else "driver-sync functional + bson unit tests"
        )
    except Exception:
        note = "driver-sync functional + bson unit tests"
    return GaugeStats(
        name="mongo-java-driver",
        language="Java",
        driver_version=_read_submodule_head("mongo-java-driver"),
        passed=passed,
        failed=failed,
        skipped=skipped,
        note=note,
    )


_COLLECTORS = (_collect_pymongo, _collect_java, _collect_go, _collect_node, _collect_ruby)


def render(raw_dir: Path, out_path: Path) -> None:
    gauges = [g for g in (c(raw_dir) for c in _COLLECTORS) if g is not None]

    total_passed = sum(g.passed for g in gauges)
    total_failed = sum(g.failed for g in gauges)
    total_skipped = sum(g.skipped for g in gauges)
    total_ran = total_passed + total_failed
    grand_rate = f"{(total_passed / total_ran * 100):.1f}%" if total_ran else "—"

    md: list[str] = []
    md.append("# Cross-Driver Conformance Summary")
    md.append("")
    md.append(
        f"Generated {dt.date.today().isoformat()} — SecantusDB {secantus.__version__}. "
        "Each per-driver gauge runs the driver vendor's own integration test suite "
        "(unmodified) against a SecantusDB daemon and emits its raw output to "
        "`.validation/`. This summary normalises on **test count** so the five gauges "
        "compare like for like — every row counts one assertion outcome, "
        "whether it landed as a JUnit `<testcase>`, a Mocha test, an RSpec example, "
        "a `go test` event, or a pytest collected item."
    )
    md.append("")
    md.append("## Summary by driver")
    md.append("")
    md.append(
        "| Driver | Language | Driver version | Tests run | Passed | Failed | Skipped | Pass rate |"
    )
    md.append("|---|---|---|---:|---:|---:|---:|---:|")
    for g in gauges:
        md.append(
            f"| `{g.name}` | {g.language} | `{g.driver_version}` | "
            f"{g.total} | {g.passed} | {g.failed} | {g.skipped} | {g.pass_rate} |"
        )
    overall_total = total_passed + total_failed + total_skipped
    md.append(
        f"| **All drivers** | — | — | **{overall_total}** | **{total_passed}** | "
        f"**{total_failed}** | **{total_skipped}** | **{grand_rate}** |"
    )
    md.append("")
    md.append("## Per-driver scope")
    md.append("")
    for g in gauges:
        md.append(f"- **`{g.name}`** — {g.note}.")
    md.append("")
    md.append("## Per-driver reports")
    md.append("")
    md.append(
        "Each gauge ships its own detailed report — per-category breakdown, "
        "named failures for triage, and the gauge's own setup notes. Open the "
        "one whose pass / fail counts you want to dig into:"
    )
    md.append("")
    md.append("- [pymongo](./validation-report.md)")
    md.append("- [mongo-java-driver](./validation-report-java.md)")
    md.append("- [mongo-go-driver](./validation-report-go.md)")
    md.append("- [mongo-node-driver](./validation-report-node.md)")
    md.append("- [mongo-ruby-driver](./validation-report-ruby.md)")
    md.append("")
    md.append("## Refreshing")
    md.append("")
    md.append(
        "Run all five gauges plus this summary:\n"
        "\n"
        "```\n"
        "uv run python -m invoke validate-all\n"
        "uv run python -m invoke validate-summary\n"
        "```\n"
        "\n"
        "Run a single gauge (still updates that one report) plus the summary:\n"
        "\n"
        "```\n"
        "uv run python -m invoke validate-java       # or validate / validate-go / etc.\n"
        "uv run python -m invoke validate-summary\n"
        "```\n"
        "\n"
        "The summary reads whatever is currently in `.validation/`; a gauge that's "
        "never been run is silently omitted from the table."
    )
    md.append("")

    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text("\n".join(md))


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--raw-dir", type=Path, default=Path(".validation"))
    parser.add_argument("--out", type=Path, default=Path("docs/validation-summary.md"))
    args = parser.parse_args()
    render(args.raw_dir, args.out)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
