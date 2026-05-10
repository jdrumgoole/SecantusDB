"""Turn mocha's JSON reporter output into docs/validation-report-node.md.

Usage:
    python -m node_validation.generate_report <raw.json> <output.md>

mocha's JSON reporter emits one big JSON document with `stats`,
`tests`, `passes`, `failures`, `pending`. Group by the first path
component under `test/` and render in the same shape as the
pymongo / go reports.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
from collections import defaultdict
from pathlib import Path

import secantus

TEST_PREFIX = "test/"


def _category_for(file: str) -> str:
    """First path component under test/ — directory or file name."""
    if not file:
        return "?"
    rel = file
    if TEST_PREFIX in rel:
        rel = rel.split(TEST_PREFIX, 1)[1]
    head = rel.split("/", 1)[0]
    return head or "?"


def _read_driver_version() -> str:
    """Read the pinned mongo-node-driver tag from the submodule."""
    head_file = Path("vendor/node-mongodb-native/.git")
    try:
        if head_file.is_file():
            modules_dir = Path(head_file.read_text().strip().removeprefix("gitdir: "))
            head = (Path("vendor/node-mongodb-native") / modules_dir / "HEAD").resolve()
            if head.exists():
                return head.read_text().strip()
    except Exception:
        pass
    return "unknown"


def render(raw_path: Path, out_path: Path) -> None:
    raw = json.loads(raw_path.read_text())

    by_cat: dict[str, dict[str, int]] = defaultdict(
        lambda: {"passed": 0, "failed": 0, "pending": 0}
    )
    failures: list[tuple[str, str]] = []  # (category, full title)

    for test in raw.get("passes", []):
        by_cat[_category_for(test.get("file", ""))]["passed"] += 1
    for test in raw.get("failures", []):
        cat = _category_for(test.get("file", ""))
        by_cat[cat]["failed"] += 1
        failures.append((cat, test.get("fullTitle", test.get("title", "?"))))
    for test in raw.get("pending", []):
        by_cat[_category_for(test.get("file", ""))]["pending"] += 1

    rows: list[tuple[str, int, int, int, int, str]] = []
    totals = {"passed": 0, "failed": 0, "pending": 0}
    for cat in sorted(by_cat):
        b = by_cat[cat]
        total = b["passed"] + b["failed"] + b["pending"]
        ran = b["passed"] + b["failed"]
        rate = f"{(b['passed'] / ran * 100):.1f}%" if ran else "—"
        rows.append((cat, b["passed"], b["failed"], b["pending"], total, rate))
        for k in totals:
            totals[k] += b[k]

    grand_total = sum(totals.values())
    grand_ran = totals["passed"] + totals["failed"]
    grand_rate = f"{(totals['passed'] / grand_ran * 100):.1f}%" if grand_ran else "—"

    md: list[str] = []
    md.append("# mongo-node-driver Validation Report")
    md.append("")
    md.append(
        f"Generated {dt.date.today().isoformat()} — SecantusDB "
        f"{secantus.__version__} vs mongo-node-driver "
        f"{_read_driver_version()[:12]} (`vendor/node-mongodb-native/`)."
    )
    md.append("")
    md.append(
        "Run `uv run python -m invoke validate-node` to refresh. The pass "
        "rate is the analogue of the pymongo / mongo-go-driver gauges for "
        "the official Node.js driver — the same driver `mongosh` and the "
        "JavaScript ecosystem build on."
    )
    md.append("")
    md.append("## Summary by category")
    md.append("")
    md.append("| Category | Passed | Failed | Pending | Total | Pass rate |")
    md.append("|---|---:|---:|---:|---:|---:|")
    for cat, p, f, pn, t, r in rows:
        md.append(f"| `{cat}` | {p} | {f} | {pn} | {t} | {r} |")
    md.append(
        f"| **Overall** | **{totals['passed']}** | **{totals['failed']}** | "
        f"**{totals['pending']}** | **{grand_total}** | **{grand_rate}** |"
    )
    md.append("")

    if failures:
        md.append(f"## Failures ({len(failures)})")
        md.append("")
        md.append("First 30 failed test titles for triage:")
        md.append("")
        md.append("```")
        for cat, title in failures[:30]:
            md.append(f"{cat} :: {title}")
        md.append("```")
        if len(failures) > 30:
            md.append(f"... and {len(failures) - 30} more (see raw JSON).")
        md.append("")

    md.append("## How this is generated")
    md.append("")
    md.append(
        "**mongo-node-driver's tests are run unmodified, against a standalone "
        "SecantusDB daemon.** The submodule at `vendor/node-mongodb-native/` "
        "is checked out at the pinned upstream tag with zero local edits. "
        "`node_validation/runner.py` ensures `node_modules/` is installed "
        "(one-time `npm install` + `npm run build:bundle`), then does a "
        "two-phase spawn: phase 1 boots `python -m secantus --port 27018 "
        "--storage-path <tempdir> --standalone` without `--auth` and uses "
        "pymongo to createUser `root-user` (root role); phase 2 stops that "
        "daemon and restarts on the same tempdir **with `--auth`**, so the "
        "user record persists and the server now enforces auth. "
        "`MONGODB_URI=mongodb://root-user:password@127.0.0.1:27018/"
        "?authSource=admin` and `AUTH=auth` make the driver's "
        "`test/tools/runner/hooks/configuration.ts` honour our URI rather "
        "than fall back to the `bob:pwd123` default. Then "
        "`npx mocha --config test/mocha_mongodb.js --reporter json <paths>`."
    )
    md.append("")
    md.append(
        "These are **integration tests** under `test/integration/` — every "
        "test opens a real `MongoClient` against the SecantusDB daemon and "
        "exchanges wire commands end-to-end. The pass rate is therefore a "
        "true measure of SecantusDB's compatibility with the Node.js "
        "driver, not of the driver's own pure-code logic."
    )
    md.append("")
    md.append(
        "The include set is currently narrow on purpose: a single broken "
        "test in a change-streams or sessions file can pin the runner "
        "indefinitely on a tailable getMore that never completes. Each "
        "new file is added to `include_paths.py` only after a manual "
        "confirmation that it terminates within the runner's wall-clock "
        "guard (600 s by default)."
    )
    md.append("")

    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text("\n".join(md))


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("raw_json", type=Path)
    parser.add_argument("output_md", type=Path)
    args = parser.parse_args()
    render(args.raw_json, args.output_md)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
