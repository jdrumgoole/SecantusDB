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
from dataclasses import dataclass, field
from pathlib import Path

import secantus
from validation_summary import expected_failures as ef_module

REPO_ROOT = Path(__file__).resolve().parent.parent


@dataclass
class GaugeStats:
    """Normalized per-gauge stats. Every gauge contributes one of these."""

    name: str  # display name, e.g. "mongo-java-driver"
    language: str  # display label, e.g. "Java"
    driver_version: str  # the upstream tag / SHA being tested against
    passed: int
    failed: int  # total failures observed by the gauge
    skipped: int
    failure_descriptions: list[str]  # the raw failure descriptions, for matching
    note: str = ""  # optional scope note (e.g. "21 of 112 functional classes")

    # Populated by ``_apply_expected_failures`` below.
    expected_failures: int = 0
    expected_failure_entries: list[tuple[str, str]] = field(default_factory=list)

    @property
    def actionable_failures(self) -> int:
        """Failures not pre-declared in ``expected_failures.py``."""
        return self.failed - self.expected_failures

    @property
    def total(self) -> int:
        return self.passed + self.failed + self.skipped

    @property
    def ran(self) -> int:
        return self.passed + self.failed

    @property
    def pass_rate(self) -> str:
        return f"{(self.passed / self.ran * 100):.1f}%" if self.ran else "—"

    @property
    def adjusted_pass_rate(self) -> str:
        """Pass rate counting expected failures as if they're not in the
        denominator — measures "how much of the conformable surface
        actually conforms."
        """
        adj_ran = self.ran - self.expected_failures
        if adj_ran <= 0:
            return "—"
        return f"{(self.passed / adj_ran * 100):.1f}%"


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


# pymongo test files that are the driver's OWN in-process unit tests —
# BSON codec, BSON corpus, type classes (ObjectId / SON / Timestamp /
# Code / DBRef), JSON util, error classes, default-export surface. They
# never open a connection to SecantusDB (verified: zero
# ``client_context`` / collection references), so they pass against any
# server — or none. Counting them would inflate the headline
# "compatibility" number with tests that don't measure SecantusDB, the
# same way the Java ``:bson:test`` module did. Excluded from the count;
# the server-touching files (including test_binary / test_raw_bson /
# test_common / test_logger / test_custom_types, which DO connect) stay.
_PYMONGO_NON_SERVER_FILES = frozenset(
    {
        "test_bson.py",
        "test_bson_corpus.py",
        "test_objectid.py",
        "test_son.py",
        "test_json_util.py",
        "test_dbref.py",
        "test_code.py",
        "test_timestamp.py",
        "test_default_exports.py",
        "test_errors.py",
    }
)


def _collect_pymongo(raw_dir: Path) -> GaugeStats | None:
    """Read ``pytest-json-report`` output (``.validation/raw.json``)."""
    f = raw_dir / "raw.json"
    if not f.exists():
        return None
    raw = json.loads(f.read_text())

    def _is_server_test(nodeid: str) -> bool:
        return nodeid.split("::")[0].split("/")[-1] not in _PYMONGO_NON_SERVER_FILES

    passed = failed = skipped = 0
    failure_descs: list[str] = []
    for t in raw.get("tests", []):
        if not _is_server_test(t.get("nodeid", "")):
            continue
        o = t.get("outcome")
        if o == "passed":
            passed += 1
        elif o in ("failed", "error"):
            failed += 1
            failure_descs.append(t["nodeid"])
        elif o == "skipped":
            skipped += 1
    # ``subtests passed`` (pytest-subtests) come only from the unified
    # spec runners, which are server-touching — add them to the count.
    passed += raw.get("summary", {}).get("subtests passed", 0)
    return GaugeStats(
        name="pymongo",
        language="Python",
        driver_version=_read_submodule_head("pymongo-tests"),
        passed=passed,
        failed=failed,
        skipped=skipped,
        failure_descriptions=failure_descs,
        note="curated server-touching pytest paths under vendor/pymongo-tests/test/",
    )


def _collect_go(raw_dir: Path) -> GaugeStats | None:
    """Read ``go test -json`` NDJSON (``.validation/go-raw.ndjson``)."""
    f = raw_dir / "go-raw.ndjson"
    if not f.exists():
        return None
    passed = failed = skipped = 0
    failure_descs: list[str] = []
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
            failure_descs.append(ev["Test"])
        elif action == "skip":
            skipped += 1
    return GaugeStats(
        name="mongo-go-driver",
        language="Go",
        driver_version=_read_submodule_head("mongo-go-driver"),
        passed=passed,
        failed=failed,
        skipped=skipped,
        failure_descriptions=failure_descs,
        note="vendor/mongo-go-driver/internal/integration/...",
    )


def _collect_node(raw_dir: Path) -> GaugeStats | None:
    """Read Mocha JSON reporter output (``.validation/node-raw.json``)."""
    f = raw_dir / "node-raw.json"
    if not f.exists():
        return None
    raw = json.loads(f.read_text())
    s = raw.get("stats", {})
    failure_descs = [t.get("fullTitle", "") for t in raw.get("failures", [])]
    return GaugeStats(
        name="mongo-node-driver",
        language="Node.js",
        driver_version=_read_submodule_head("node-mongodb-native"),
        passed=s.get("passes", 0),
        failed=s.get("failures", 0),
        skipped=s.get("pending", 0),
        failure_descriptions=failure_descs,
        note="curated test/integration/ spec set",
    )


def _collect_ruby(raw_dir: Path) -> GaugeStats | None:
    """Read RSpec JSON formatter output (``.validation/ruby-raw.json``)."""
    f = raw_dir / "ruby-raw.json"
    if not f.exists():
        return None
    raw = json.loads(f.read_text())
    # address_spec.rb / config_spec.rb are client-side-only unit specs
    # (Address parsing, Config) — zero collection / client references,
    # never reach the daemon. Exclude from the count so the number
    # reflects server-touching specs only.
    _NON_SERVER = {"address_spec.rb", "config_spec.rb"}
    passed = failed = skipped = 0
    failure_descs: list[str] = []
    for e in raw.get("examples", []):
        if e.get("file_path", "").split("/")[-1] in _NON_SERVER:
            continue
        st = e.get("status")
        if st == "passed":
            passed += 1
        elif st == "failed":
            failed += 1
            failure_descs.append(e.get("full_description", ""))
        elif st == "pending":
            skipped += 1
    return GaugeStats(
        name="mongo-ruby-driver",
        language="Ruby",
        driver_version=_read_submodule_head("mongo-ruby-driver"),
        passed=passed,
        failed=failed,
        skipped=skipped,
        failure_descriptions=failure_descs,
        note="curated spec/mongo/*.rb spec files",
    )


def _collect_java(raw_dir: Path) -> GaugeStats | None:
    """Walk JUnit XML output (``.validation/java-results/<module>/TEST-*.xml``)."""
    xml_dir = raw_dir / "java-results"
    if not xml_dir.is_dir():
        return None
    passed = failed = skipped = 0
    failure_descs: list[str] = []
    for xml in xml_dir.rglob("TEST-*.xml"):
        # Exclude the ``:bson:test`` module: those ~3,800 cases are
        # pure in-process BSON codec unit tests (BitsTest,
        # BasicBSONDecoderSpecification, ...) that never open a
        # connection to SecantusDB. Counting them would inflate the
        # compatibility number ~10x with tests that pass against any
        # server — or none. Only the driver-sync / driver-core
        # integration tests actually exercise SecantusDB's wire path,
        # which is what the per-gauge report already measures.
        if xml.parent.name == "bson":
            continue
        try:
            root = ET.parse(xml).getroot()
        except ET.ParseError:
            continue
        for case in root.iter("testcase"):
            if case.find("failure") is not None or case.find("error") is not None:
                failed += 1
                failure_descs.append(case.attrib.get("name", ""))
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
            f"{included} of {total_classes} driver-sync functional classes "
            "(bson codec unit tests excluded — they don't touch the server)"
            if total_classes
            else "driver-sync functional integration tests"
        )
    except Exception:
        note = "driver-sync functional integration tests"
    return GaugeStats(
        name="mongo-java-driver",
        language="Java",
        driver_version=_read_submodule_head("mongo-java-driver"),
        passed=passed,
        failed=failed,
        skipped=skipped,
        failure_descriptions=failure_descs,
        note=note,
    )


def _collect_rust(raw_dir: Path) -> GaugeStats | None:
    """Read the rust gauge's parsed cargo output (``.validation/rust-raw.json``)."""
    f = raw_dir / "rust-raw.json"
    if not f.exists():
        return None
    raw = json.loads(f.read_text())
    s = raw.get("summary", {})
    failed = s.get("failed", 0)
    skipped = s.get("ignored", 0)
    passed = s.get("passed", 0)
    failure_descs = [t.get("name", "") for t in raw.get("failures", [])]
    return GaugeStats(
        name="mongo-rust-driver",
        language="Rust",
        driver_version=_read_submodule_head("mongo-rust-driver"),
        passed=passed,
        failed=failed,
        skipped=skipped,
        failure_descriptions=failure_descs,
        note="curated driver/src/test/ in-tree tests",
    )


def _collect_php_ext(raw_dir: Path) -> GaugeStats | None:
    """Walk run-tests.php JUnit XML (``.validation/php-ext-junit.xml``).

    Excludes the ``tests/bson`` directory: those ~440 cases are pure
    in-process BSON serialization tests that never open a connection to
    SecantusDB (same rationale as the Java ``:bson:test`` exclusion).
    Only the wire-protocol directories measure SecantusDB's command
    surface, which is what the compatibility number should reflect.
    """
    f = raw_dir / "php-ext-junit.xml"
    if not f.exists():
        return None
    try:
        root = ET.parse(f).getroot()
    except ET.ParseError:
        return None
    passed = failed = skipped = 0
    failure_descs: list[str] = []
    for case in root.iter("testcase"):
        name = case.attrib.get("name", "")
        # Category is the path component after ``tests/`` in the name.
        cat = name.split("tests/", 1)[1].split("/", 1)[0] if "tests/" in name else ""
        if cat == "bson":
            continue
        if case.find("failure") is not None or case.find("error") is not None:
            failed += 1
            failure_descs.append(name)
        elif case.find("skipped") is not None:
            skipped += 1
        else:
            passed += 1
    return GaugeStats(
        name="mongo-php-driver",
        language="PHP",
        driver_version=_read_submodule_head("mongo-php-driver"),
        passed=passed,
        failed=failed,
        skipped=skipped,
        failure_descriptions=failure_descs,
        note="curated .phpt wire-protocol tests (bson serialization units excluded)",
    )


# php-library categories that open a real connection to SecantusDB. The
# pure-code units (Builder / Comparator / Functions / Model — query-builder
# DSL, BSON comparators, helper functions) never touch the server, so they're
# excluded from the compatibility number the same way Java's bson codec units
# are.
_PHP_LIB_SERVER_CATEGORIES = frozenset({"Operation", "Collection", "Database", "Command"})


def _collect_php_lib(raw_dir: Path) -> GaugeStats | None:
    """Walk PHPUnit JUnit XML (``.validation/php-lib-junit.xml``).

    Counts only the server-touching functional categories (see
    ``_PHP_LIB_SERVER_CATEGORIES``); the pure-code DSL / comparator /
    helper units are run but not counted, mirroring the Java gauge.
    """
    f = raw_dir / "php-lib-junit.xml"
    if not f.exists():
        return None
    try:
        root = ET.parse(f).getroot()
    except ET.ParseError:
        return None

    def _category(case: ET.Element) -> str:
        file = case.attrib.get("file", "")
        if "/tests/" in file:
            return file.split("/tests/", 1)[1].split("/", 1)[0]
        cls = case.attrib.get("class", "")
        parts = cls.split("\\")
        if "Tests" in parts:
            i = parts.index("Tests")
            if i + 1 < len(parts):
                return parts[i + 1]
        return ""

    passed = failed = skipped = 0
    failure_descs: list[str] = []
    for case in root.iter("testcase"):
        if _category(case) not in _PHP_LIB_SERVER_CATEGORIES:
            continue
        name = f"{case.attrib.get('class', '?')}::{case.attrib.get('name', '?')}"
        if case.find("failure") is not None or case.find("error") is not None:
            failed += 1
            failure_descs.append(name)
        elif case.find("skipped") is not None:
            skipped += 1
        else:
            passed += 1
    return GaugeStats(
        name="mongo-php-library",
        language="PHP",
        driver_version=_read_submodule_head("mongo-php-library"),
        passed=passed,
        failed=failed,
        skipped=skipped,
        failure_descriptions=failure_descs,
        note="curated functional tests (Operation / Collection / Database / Command)",
    )


_COLLECTORS = (
    _collect_pymongo,
    _collect_java,
    _collect_go,
    _collect_node,
    _collect_ruby,
    _collect_rust,
    _collect_php_lib,
    _collect_php_ext,
)

# Gauge name -> its per-driver report page (relative to docs/). Used by the
# "Per-driver reports" link list; a gauge without an entry is omitted.
_REPORT_LINKS = {
    "pymongo": "./validation-report.md",
    "mongo-java-driver": "./validation-report-java.md",
    "mongo-go-driver": "./validation-report-go.md",
    "mongo-node-driver": "./validation-report-node.md",
    "mongo-ruby-driver": "./validation-report-ruby.md",
    "mongo-rust-driver": "./validation-report-rust.md",
    "mongo-php-library": "./validation-report-php-lib.md",
    "mongo-php-driver": "./validation-report-php-ext.md",
}


# Map gauge ``name`` to the matching expected-failures list.
_EXPECTED_FAILURES_BY_GAUGE: dict[str, list[ef_module.ExpectedFailure]] = {
    "pymongo": ef_module.PYMONGO,
    "mongo-java-driver": ef_module.JAVA,
    "mongo-go-driver": ef_module.GO,
    "mongo-node-driver": ef_module.NODE,
    "mongo-ruby-driver": ef_module.RUBY,
}


def _apply_expected_failures(g: GaugeStats) -> None:
    """Mark each failure that matches an expected-failure pattern.

    Mutates ``g`` in place: bumps ``expected_failures`` count and
    records the ``(description, rationale)`` pair so the report can
    list them inline.
    """
    expected_list = _EXPECTED_FAILURES_BY_GAUGE.get(g.name, [])
    for desc in g.failure_descriptions:
        ef = ef_module.find_match(expected_list, desc)
        if ef is not None:
            g.expected_failures += 1
            g.expected_failure_entries.append((desc, ef.rationale))


def render(raw_dir: Path, out_path: Path) -> None:
    gauges = [g for g in (c(raw_dir) for c in _COLLECTORS) if g is not None]
    for g in gauges:
        _apply_expected_failures(g)

    total_passed = sum(g.passed for g in gauges)
    total_failed = sum(g.failed for g in gauges)
    total_skipped = sum(g.skipped for g in gauges)
    total_expected = sum(g.expected_failures for g in gauges)
    total_actionable = total_failed - total_expected
    total_ran = total_passed + total_failed
    grand_rate = f"{(total_passed / total_ran * 100):.1f}%" if total_ran else "—"
    adj_ran = total_ran - total_expected
    adj_rate = f"{(total_passed / adj_ran * 100):.1f}%" if adj_ran > 0 else "—"

    md: list[str] = []
    md.append("# Cross-Driver Conformance Summary")
    md.append("")
    md.append(
        f"Generated {dt.date.today().isoformat()} — SecantusDB {secantus.__version__}. "
        "Each per-driver gauge runs the driver vendor's own integration test suite "
        "(unmodified) against a SecantusDB daemon and emits its raw output to "
        f"`.validation/`. This summary normalises on **test count** so the {len(gauges)} gauges "
        "compare like for like — every row counts one assertion outcome, "
        "whether it landed as a JUnit `<testcase>`, a Mocha test, an RSpec example, "
        "a `go test` event, or a pytest collected item."
    )
    md.append("")
    md.append(
        "**Failures split into two columns**: *Failed* counts tests that "
        "actually need a fix on SecantusDB; *Expected* counts tests with a "
        "documented reason for failing (driver-side cascade, out-of-scope "
        "feature, single-node-topology assumption, known intermittent flake). "
        "The expected list lives in `validation_summary/expected_failures.py` "
        "and each entry carries a rationale. Adjusted pass rate = passes ÷ "
        "(passes + actual failures)."
    )
    md.append("")
    md.append("## Summary by driver")
    md.append("")
    header_cols = [
        "Driver",
        "Language",
        "Driver version",
        "Tests run",
        "Passed",
        "Failed",
        "Expected",
        "Skipped",
        "Pass rate",
        "Adjusted",
    ]
    md.append("| " + " | ".join(header_cols) + " |")
    md.append("|---|---|---|---:|---:|---:|---:|---:|---:|---:|")
    for g in gauges:
        md.append(
            f"| `{g.name}` | {g.language} | `{g.driver_version}` | "
            f"{g.total} | {g.passed} | {g.actionable_failures} | "
            f"{g.expected_failures} | {g.skipped} | {g.pass_rate} | "
            f"{g.adjusted_pass_rate} |"
        )
    overall_total = total_passed + total_failed + total_skipped
    md.append(
        f"| **All drivers** | — | — | **{overall_total}** | **{total_passed}** | "
        f"**{total_actionable}** | **{total_expected}** | **{total_skipped}** | "
        f"**{grand_rate}** | **{adj_rate}** |"
    )
    md.append("")
    md.append("## Per-driver scope")
    md.append("")
    for g in gauges:
        md.append(f"- **`{g.name}`** — {g.note}.")
    md.append("")
    if total_expected > 0:
        md.append("## Expected failures")
        md.append("")
        md.append(
            "These tests fail for documented reasons that have no SecantusDB-"
            "side fix (driver-internal behaviour we can't influence, features "
            "intentionally out of scope, single-node topology assumptions in "
            "tests that assume a 3-node replica set, etc.). Each entry has a "
            "rationale in `validation_summary/expected_failures.py`. If you fix "
            "one of these gaps, delete its entry there."
        )
        md.append("")
        for g in gauges:
            if not g.expected_failure_entries:
                continue
            md.append(f"### `{g.name}` ({len(g.expected_failure_entries)})")
            md.append("")
            for desc, rationale in g.expected_failure_entries:
                md.append(f"- **{desc}** — {rationale}")
            md.append("")
    md.append("## Per-driver reports")
    md.append("")
    md.append(
        "Each gauge ships its own detailed report — per-category breakdown, "
        "named failures for triage, and the gauge's own setup notes. Open the "
        "one whose pass / fail counts you want to dig into:"
    )
    md.append("")
    for g in gauges:
        report = _REPORT_LINKS.get(g.name)
        if report:
            md.append(f"- [{g.name}]({report})")
    md.append("")
    md.append("## Refreshing")
    md.append("")
    md.append(
        f"Run all {len(gauges)} gauges plus this summary:\n"
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
