"""Render ``dotnet test`` TRX output into ``docs/validation-report-dotnet.md``.

Same shape as the other gauges' report generators: a per-namespace table of
passed / failed / skipped counts, an overall row, and a truncated failures
list for triage.

``dotnet test --logger trx`` writes a TRX file (Visual Studio Test Results XML,
namespace ``http://microsoft.com/schemas/VisualStudio/TeamTest/2010``):
``<Results><UnitTestResult testName="Ns.Class.Method" outcome="Passed|Failed|
NotExecuted"/>`` plus a ``<TestDefinitions>`` mapping each test to its
``className``. We group by the test class's namespace.

Usage::

    uv run python -m dotnet_validation.generate_report \\
        .validation/dotnet-raw.trx docs/validation-report-dotnet.md
"""

from __future__ import annotations

import datetime as _dt
import subprocess
import sys
import xml.etree.ElementTree as ET
from collections import defaultdict
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
VENDOR = REPO_ROOT / "vendor" / "mongo-csharp-driver"
_TRX_NS = "{http://microsoft.com/schemas/VisualStudio/TeamTest/2010}"


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


# outcome -> our bucket. NotExecuted / Inconclusive / NotRunnable are xUnit
# skips; Failed / Timeout / Aborted / Error are failures.
_PASS = {"Passed"}
_SKIP = {"NotExecuted", "Inconclusive", "NotRunnable", "Pending", "Warning"}


def _namespace_of(class_name: str) -> str:
    """``MongoDB.Driver.Tests.Linq.FooTests`` -> ``MongoDB.Driver.Tests.Linq``.

    Group label = the class's namespace (className minus the final type name)."""
    if "." not in class_name:
        return class_name or "other"
    return class_name.rsplit(".", 1)[0]


def render(trx_path: Path, out_path: Path) -> None:
    root = ET.parse(trx_path).getroot()

    # testId -> className, from <TestDefinitions><UnitTest><TestMethod className=.../>
    class_by_id: dict[str, str] = {}
    for ut in root.iter(f"{_TRX_NS}UnitTest"):
        tid = ut.attrib.get("id", "")
        tm = ut.find(f"{_TRX_NS}TestMethod")
        if tm is not None:
            class_by_id[tid] = tm.attrib.get("className", "")

    by_group: dict[str, dict[str, int]] = defaultdict(
        lambda: {"passed": 0, "failed": 0, "skipped": 0}
    )
    failures: list[str] = []
    for res in root.iter(f"{_TRX_NS}UnitTestResult"):
        outcome = res.attrib.get("outcome", "")
        name = res.attrib.get("testName", "?")
        cls = class_by_id.get(res.attrib.get("testId", ""), "")
        group = _namespace_of(cls) if cls else name.rsplit(".", 1)[0]
        if outcome in _PASS:
            by_group[group]["passed"] += 1
        elif outcome in _SKIP:
            by_group[group]["skipped"] += 1
        else:
            by_group[group]["failed"] += 1
            failures.append(name)

    totals = {"passed": 0, "failed": 0, "skipped": 0}
    for b in by_group.values():
        for k in totals:
            totals[k] += b[k]
    grand_total = sum(totals.values())
    grand_ran = totals["passed"] + totals["failed"]
    grand_rate = f"{(totals['passed'] / grand_ran * 100):.1f}%" if grand_ran else "—"

    md: list[str] = []
    md.append("# mongo-csharp-driver Validation Report")
    md.append("")
    md.append(
        f"Generated {_dt.date.today().isoformat()} — "
        f"SecantusDB {_secantus_version()} vs mongo-csharp-driver "
        f"{_vendor_ref()} (`vendor/mongo-csharp-driver/`)."
    )
    md.append("")
    md.append(
        "Run `uv run python -m invoke validate-dotnet` to refresh. The official "
        "MongoDB **C# / .NET** driver — its xUnit integration suite "
        "(`MongoDB.Driver.Tests`) run unmodified against an embedded SecantusDB "
        "daemon via `dotnet test`."
    )
    md.append("")
    md.append("## Summary by namespace")
    md.append("")
    md.append("| Namespace | Passed | Failed | Skipped | Total | Pass rate |")
    md.append("|---|---:|---:|---:|---:|---:|")
    for group in sorted(by_group):
        b = by_group[group]
        total = b["passed"] + b["failed"] + b["skipped"]
        ran = b["passed"] + b["failed"]
        rate = f"{(b['passed'] / ran * 100):.1f}%" if ran else "—"
        md.append(f"| `{group}` | {b['passed']} | {b['failed']} | {b['skipped']} | {total} | {rate} |")
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
        for f in failures[:30]:
            md.append(f)
        md.append("```")
        if len(failures) > 30:
            md.append(f"... and {len(failures) - 30} more (see the TRX XML).")
        md.append("")
    md.append("## How this is generated")
    md.append("")
    md.append(
        "`invoke validate-dotnet` spawns a SecantusDB daemon on a fresh "
        "ephemeral port and runs the mongo-csharp-driver xUnit integration "
        "project via `dotnet test` with `MONGODB_URI` pointed at the daemon and "
        "Catch2-style out-of-scope categories excluded via `--filter` (see "
        "`dotnet_validation/include_paths.py`), writing TRX results that this "
        "script renders."
    )
    md.append("")
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text("\n".join(md))


def main(argv: list[str]) -> int:
    if len(argv) != 3:
        print("Usage: generate_report.py <trx> <out.md>", file=sys.stderr)
        return 2
    render(Path(argv[1]), Path(argv[2]))
    print(f"Wrote {argv[2]}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
