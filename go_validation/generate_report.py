"""Turn `go test -json` NDJSON into docs/validation-report-go.md.

Usage:
    python -m go_validation.generate_report <ndjson> <output.md>

Groups by Go package (the `Package` field on each event) and emits the
same shape of markdown table as pymongo_validation/generate_report.py
so the two reports look at home next to each other.
"""

from __future__ import annotations

import argparse
import json
import sys
from collections import defaultdict
from pathlib import Path

import gauge_common
from validation_summary.rates import pass_rate

import secantus

VENDOR_PREFIX = "go.mongodb.org/mongo-driver/v2/"


def _shorten(pkg: str) -> str:
    if pkg.startswith(VENDOR_PREFIX):
        return pkg[len(VENDOR_PREFIX) :]
    return pkg


def _read_driver_version() -> str:
    """Best-effort read of the pinned mongo-go-driver tag."""
    head_file = Path("vendor/mongo-go-driver/.git")
    try:
        if head_file.is_file():
            modules_dir = Path(head_file.read_text().strip().removeprefix("gitdir: "))
            head = (Path("vendor/mongo-go-driver") / modules_dir / "HEAD").resolve()
            if head.exists():
                return head.read_text().strip()
    except Exception:
        pass
    return "unknown"


def render(ndjson_path: Path, out_path: Path) -> bool:
    """Write the report. Returns True when the run was TRUNCATED.

    A truncated run is the dangerous case, because it looks like a clean one.
    `go test` killing itself on `-timeout` panics the binary WITHOUT emitting a
    terminal event for the tests still in flight, so the summariser sees only
    the tests that finished, counts zero failures among them, and prints
    100.0%. The package-level result is `fail` at the same time — the two
    disagree, and only the raw NDJSON shows it.

    That is not hypothetical: every recorded run of this gauge, on both servers
    and including the committed 2026-09-21 report, stopped at 476 of 481 tests
    after a 30-minute timeout in `TestInitialDNSSeedlistDiscoverySpec` (a DNS
    resolver test that never contacts SecantusDB), and all of them published
    100.0%. Two artifacts from different servers a week apart agreeing exactly
    read as confirmation; it meant both were cut off at the same hang.
    """
    by_pkg: dict[str, dict[str, int]] = defaultdict(
        lambda: {"passed": 0, "failed": 0, "skipped": 0}
    )

    failures: list[tuple[str, str]] = []  # (package, test) for triage
    started: set[str] = set()
    finished: set[str] = set()
    pkg_failed: set[str] = set()  # package-level `fail`, test-level or not

    with ndjson_path.open() as f:
        for line in f:
            try:
                ev = json.loads(line)
            except json.JSONDecodeError:
                continue
            action = ev.get("Action")
            test = ev.get("Test")
            pkg = ev.get("Package", "?")
            if not test:
                # Package-level roll-up. `fail` here with no failing test below
                # it is the signature of a binary that died without accounting
                # for its tests.
                if action == "fail":
                    pkg_failed.add(pkg)
                continue
            if action == "run":
                started.add(f"{pkg}::{test}")
                continue
            # Only count terminal events on individual tests (skip subtest
            # roll-ups; `go test -json` emits an event per test name).
            if action not in {"pass", "fail", "skip"}:
                continue
            finished.add(f"{pkg}::{test}")
            bucket = {"pass": "passed", "fail": "failed", "skip": "skipped"}[action]
            by_pkg[pkg][bucket] += 1
            if action == "fail":
                failures.append((pkg, test))

    hung = sorted(started - finished)
    silent_pkg_failures = sorted(p for p in pkg_failed if not any(fp == p for fp, _ in failures))
    truncated = bool(hung or silent_pkg_failures)

    rows: list[tuple[str, int, int, int, int, str]] = []
    totals = {"passed": 0, "failed": 0, "skipped": 0}
    for pkg in sorted(by_pkg):
        b = by_pkg[pkg]
        total = b["passed"] + b["failed"] + b["skipped"]
        ran = b["passed"] + b["failed"]
        rate = pass_rate(b["passed"], ran)
        rows.append((_shorten(pkg), b["passed"], b["failed"], b["skipped"], total, rate))
        for k in totals:
            totals[k] += b[k]

    grand_total = sum(totals.values())
    grand_ran = totals["passed"] + totals["failed"]
    grand_rate = pass_rate(totals["passed"], grand_ran)

    md: list[str] = []
    md.append("# mongo-go-driver Validation Report")
    md.append("")
    md.append(
        f"{gauge_common.measured_on()} — SecantusDB "
        f"{secantus.__version__} vs mongo-go-driver "
        f"{_read_driver_version()[:12]} (`vendor/mongo-go-driver/`)."
    )
    md.append("")
    md.append(
        "Run `uv run python -m invoke validate-go` to refresh. The pass rate "
        "is the analogue of the pymongo conformance gauge for the official "
        "Go driver — same shape, different wire-protocol pickiness. Type-strict "
        "bugs (int32 vs int64) that pymongo accepts silently fail loudly here."
    )
    md.append("")

    if truncated:
        # Ahead of the numbers on purpose. A reader who sees the table first
        # has already formed a view of the pass rate by the time any caveat
        # further down reaches them.
        md.append("> **THIS RUN WAS TRUNCATED — the pass rate below is not a**")
        md.append("> **conformance result.** The test binary stopped before")
        md.append("> every test reported, so the rate describes only the")
        md.append("> subset that finished. Fix the cause and re-run before")
        md.append("> quoting any number from this file.")
        md.append(">")
        if hung:
            md.append(f"> Started but never completed ({len(hung)}):")
            md.append(">")
            md.append("> ```")
            for t in hung[:20]:
                md.append(f"> {t}")
            md.append("> ```")
        if silent_pkg_failures:
            md.append(
                "> Packages reporting `fail` with no failing test beneath them "
                f"(a binary that died without accounting for its tests): "
                f"{', '.join(_shorten(p) for p in silent_pkg_failures)}"
            )
        md.append("")

    md.append("## Summary by package")
    md.append("")
    md.append("| Package | Passed | Failed | Skipped | Total | Pass rate |")
    md.append("|---|---:|---:|---:|---:|---:|")
    for pkg, p, f, s, t, r in rows:
        md.append(f"| `{pkg}` | {p} | {f} | {s} | {t} | {r} |")
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
        for pkg, test in failures[:30]:
            md.append(f"{_shorten(pkg)} :: {test}")
        md.append("```")
        if len(failures) > 30:
            md.append(f"... and {len(failures) - 30} more (see raw NDJSON).")
        md.append("")

    md.append("## How this is generated")
    md.append("")
    md.append(
        "**mongo-go-driver's integration tests are run unmodified, against "
        "a standalone SecantusDB daemon.** The submodule at "
        "`vendor/mongo-go-driver/` is checked out at the pinned upstream "
        "tag with zero local edits. `go_validation/runner.py` spawns "
        "`python -m secantus --host 127.0.0.1 --port 27018 --storage-path "
        "<tempdir> --noop-heartbeat-seconds 10` as a subprocess (a fresh "
        "`tempfile.mkdtemp(prefix='secantus-go-gauge-')` — never "
        "`:memory:`; on-disk WiredTiger keeps the checkpoint / journal "
        "code paths exercised), waits "
        "for its TCP listener, exports `MONGODB_URI=mongodb://"
        "127.0.0.1:27018` (the env var `internal/integtest.MongoDBURI` "
        "and `internal/integration/mtest` read at setup), then runs "
        "`go test -json -count=1 ./internal/integration/...`. From the "
        "go-driver's point of view it's connecting to a real `mongod` "
        "over TCP — exactly like its CI does."
    )
    md.append("")
    md.append(
        "**Integration-only.** The pure-BSON unit tests under "
        "`./bson/...` and `./mongo` are out of scope for this gauge — "
        "they verify the driver's own serialization logic without ever "
        "opening a TCP connection, and would inflate the pass count "
        "without proving anything about SecantusDB's wire path. The "
        "pass rate above is a true measure of cross-driver compatibility "
        "with the language-canonical Go driver `mongodump` and "
        "`mongorestore` are built on."
    )
    md.append("")
    md.append(
        "Tests gated on topology (`mtest.RequiresReplicaSet`, "
        "`mtest.RequiresSharded`, etc.) self-skip when the server "
        "doesn't match — those skips are honest gaps, not failures."
    )
    md.append("")

    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text("\n".join(md))
    return truncated


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("ndjson", type=Path)
    parser.add_argument("output_md", type=Path)
    args = parser.parse_args()
    truncated = render(args.ndjson, args.output_md)
    if truncated:
        # Non-zero so the gauge cannot be reported as a clean run. The report
        # is still written -- it carries the banner and the hung-test list,
        # which is what you need to diagnose it.
        print(
            f"go gauge TRUNCATED: not every test reported a result; see "
            f"{args.output_md}. The pass rate in that file is not a "
            f"conformance number.",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
