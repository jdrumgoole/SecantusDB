"""Render the sql-stress smoke's raw lane results into
docs/validation-report-sqlstress.md.

Usage:
    python -m sqlstress_validation.generate_report <raw.json> <output.md>
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import re
from pathlib import Path


def _secantus_version() -> str:
    try:
        import secantus

        return secantus.__version__
    except ImportError:
        init = Path(__file__).resolve().parent.parent / "src" / "secantus" / "__init__.py"
        m = re.search(r'__version__ = "([^"]+)"', init.read_text())
        return m.group(1) if m else "unknown"


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("raw_json")
    parser.add_argument("output_md")
    args = parser.parse_args()

    data = json.loads(Path(args.raw_json).read_text())
    lanes = data.get("lanes", [])
    ok = sum(1 for lane in lanes if lane["ok"])
    lines = [
        "# pgbench + psql stress/smoke report",
        "",
        f"- SecantusDB (Python server) {_secantus_version()}",
        "- pgbench TPC-B (simple / extended / prepared) + select-only + psql catalog smoke",
        f"- generated: {dt.datetime.now(dt.timezone.utc).strftime('%Y-%m-%d %H:%M UTC')}",
        "",
        f"**{ok}/{len(lanes)} lanes clean.** Any error or dropped connection is a bug;",
        "tps figures are smoke-level indicators, not benchmarks.",
        "",
        "| lane | status | tps |",
        "|---|---|---|",
    ]
    for lane in lanes:
        status = "ok" if lane["ok"] else f"FAIL — {lane.get('detail') or ''}"
        tps = f"{lane['tps']:.0f}" if lane.get("tps") else "—"
        lines.append(f"| {lane['lane']} | {status} | {tps} |")

    Path(args.output_md).write_text("\n".join(lines) + "\n")
    print(f"sql-stress: {ok}/{len(lanes)} lanes clean")


if __name__ == "__main__":
    main()
