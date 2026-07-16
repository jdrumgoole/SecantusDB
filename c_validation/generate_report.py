"""Render ``test-libmongoc`` JSON into ``docs/validation-report-c.md``.

Same shape as the other gauges' report generators
(``rust_validation/generate_report.py`` et al.): a per-suite table of
passed / failed / skipped counts, an overall row, and a truncated
failures list for triage.

The libmongoc runner writes ``{"results": [{"status": "pass"|"fail"|
"skip", "test_file": "/Suite/test", ...}, ...]}`` (see
``src/libmongoc/tests/TestSuite.c``). The "suite" is the first path
component of ``test_file``.

Usage::

    uv run python -m c_validation.generate_report \\
        .validation/c-raw.json docs/validation-report-c.md
"""

from __future__ import annotations

import datetime as _dt
import subprocess
import sys
from pathlib import Path

from c_validation import load_results

REPO_ROOT = Path(__file__).resolve().parent.parent
VENDOR = REPO_ROOT / "vendor" / "mongo-c-driver"


def _vendor_ref() -> str:
    try:
        out = subprocess.run(
            ["git", "-C", str(VENDOR), "rev-parse", "--short", "HEAD"],
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


def _suite_of(test_file: str) -> str:
    """First path component of ``/Suite/sub/test`` → ``Suite``."""
    parts = [p for p in test_file.split("/") if p]
    return parts[0] if parts else "other"


def _render(raw: dict) -> str:
    results = raw.get("results", [])
    by_suite: dict[str, dict[str, int]] = {}
    failures: list[str] = []
    tot = {"passed": 0, "failed": 0, "skipped": 0}
    for t in results:
        status = (t.get("status") or "").lower()
        name = t.get("test_file", "")
        bucket = by_suite.setdefault(
            _suite_of(name), {"passed": 0, "failed": 0, "skipped": 0}
        )
        if status == "pass":
            bucket["passed"] += 1
            tot["passed"] += 1
        elif status == "fail":
            bucket["failed"] += 1
            tot["failed"] += 1
            failures.append(name)
        elif status == "skip":
            bucket["skipped"] += 1
            tot["skipped"] += 1
    total_overall = tot["passed"] + tot["failed"] + tot["skipped"]
    denom_overall = tot["passed"] + tot["failed"]
    rate_overall = 100.0 * tot["passed"] / max(1, denom_overall) if denom_overall else 100.0

    lines: list[str] = []
    lines.append("# mongo-c-driver Validation Report")
    lines.append("")
    lines.append(
        f"Generated {_dt.date.today().isoformat()} — "
        f"SecantusDB {_secantus_version()} vs mongo-c-driver "
        f"{_vendor_ref()} (`vendor/mongo-c-driver/`)."
    )
    lines.append("")
    lines.append(
        "Run `uv run python -m invoke validate-c` to refresh. The official "
        "MongoDB **C** driver (`libmongoc`) is the lowest-level official "
        "client — and (with the Go and PHP-extension gauges) one of the "
        "strictest wire-protocol checks."
    )
    lines.append("")
    lines.append("## Summary")
    lines.append("")
    lines.append("| Suite | Passed | Failed | Skipped | Total | Pass rate |")
    lines.append("|---|---:|---:|---:|---:|---:|")
    for suite in sorted(by_suite):
        b = by_suite[suite]
        total = b["passed"] + b["failed"] + b["skipped"]
        denom = b["passed"] + b["failed"]
        rate = 100.0 * b["passed"] / max(1, denom) if denom else 100.0
        lines.append(
            f"| `/{suite}` | {b['passed']} | {b['failed']} | "
            f"{b['skipped']} | {total} | {rate:.1f}% |"
        )
    lines.append(
        f"| **Overall** | **{tot['passed']}** | **{tot['failed']}** | "
        f"**{tot['skipped']}** | **{total_overall}** | **{rate_overall:.1f}%** |"
    )
    lines.append("")
    if failures:
        lines.append(f"## Failures ({len(failures)})")
        lines.append("")
        lines.append("First 30 failed tests for triage:")
        lines.append("")
        lines.append("```")
        for name in failures[:30]:
            lines.append(name)
        lines.append("```")
        lines.append("")
    lines.append("## How this is generated")
    lines.append("")
    lines.append(
        "`invoke validate-c` builds the vendored driver's `test-libmongoc` "
        "binary once (CMake, `ENABLE_TESTS=ON`), spawns a SecantusDB daemon "
        "on a fresh ephemeral port, and runs the curated `-l` prefixes with "
        "`MONGOC_TEST_URI` pointed at the daemon, writing JSON results via "
        "`-F`. The list of in-scope test prefixes (and the skip-list of "
        "out-of-scope tests) lives in `c_validation/include_paths.py`."
    )
    lines.append("")
    return "\n".join(lines)


def main(argv: list[str]) -> int:
    if len(argv) != 3:
        print("Usage: generate_report.py <raw.json> <out.md>", file=sys.stderr)
        return 2
    raw = load_results(argv[1])
    out = Path(argv[2])
    out.write_text(_render(raw))
    print(f"Wrote {out}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
