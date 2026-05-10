"""Turn rspec --format json output into docs/validation-report-ruby.md.

Usage:
    python -m ruby_validation.generate_report <raw.json> <output.md>

rspec emits one JSON document with ``examples`` (each test) and
``summary`` (counts + duration). Group by the first path component
under ``spec/`` and render in the same shape as the pymongo / Go /
Node / Java reports.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
from collections import defaultdict
from pathlib import Path

import secantus

SPEC_PREFIX = "./spec/"


def _category_for(file: str) -> str:
    if not file:
        return "?"
    rel = file
    if SPEC_PREFIX in rel:
        rel = rel.split(SPEC_PREFIX, 1)[1]
    elif rel.startswith("spec/"):
        rel = rel[len("spec/") :]
    head = rel.split("/", 1)[0]
    return head or "?"


def _read_driver_version() -> str:
    head_file = Path("vendor/mongo-ruby-driver/.git")
    try:
        if head_file.is_file():
            modules_dir = Path(head_file.read_text().strip().removeprefix("gitdir: "))
            head = (Path("vendor/mongo-ruby-driver") / modules_dir / "HEAD").resolve()
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
    failures: list[tuple[str, str]] = []

    for ex in raw.get("examples", []):
        cat = _category_for(ex.get("file_path", ""))
        status = ex.get("status", "?")
        if status == "passed":
            by_cat[cat]["passed"] += 1
        elif status == "failed":
            by_cat[cat]["failed"] += 1
            failures.append((cat, ex.get("full_description", ex.get("description", "?"))))
        elif status == "pending":
            by_cat[cat]["pending"] += 1

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

    summary = raw.get("summary", {})
    duration = float(summary.get("duration", 0.0))

    md: list[str] = []
    md.append("# mongo-ruby-driver Validation Report")
    md.append("")
    md.append(
        f"Generated {dt.date.today().isoformat()} — SecantusDB "
        f"{secantus.__version__} vs mongo-ruby-driver "
        f"{_read_driver_version()[:12]} (`vendor/mongo-ruby-driver/`)."
    )
    md.append("")
    md.append(
        "Run `uv run python -m invoke validate-ruby` to refresh. The pass "
        "rate is the analogue of the pymongo / mongo-go-driver / "
        "mongo-node-driver / mongo-java-driver gauges for the official "
        "Ruby driver — the same gem Rails + Sinatra applications and the "
        "Ruby ecosystem build on."
    )
    md.append("")
    md.append("## Summary by category")
    md.append("")
    md.append("| Category | Passed | Failed | Pending | Total | Pass rate |")
    md.append("|---|---:|---:|---:|---:|---:|")
    for cat, p, f, pn, t, r in rows:
        md.append(f"| `spec/{cat}` | {p} | {f} | {pn} | {t} | {r} |")
    md.append(
        f"| **Overall** | **{totals['passed']}** | **{totals['failed']}** | "
        f"**{totals['pending']}** | **{grand_total}** | **{grand_rate}** |"
    )
    md.append("")
    md.append(f"Run time: {duration:.2f}s.")
    md.append("")

    if failures:
        md.append(f"## Failures ({len(failures)})")
        md.append("")
        md.append("First 30 failed examples for triage:")
        md.append("")
        md.append("```")
        for cat, title in failures[:30]:
            md.append(f"spec/{cat} :: {title}")
        md.append("```")
        if len(failures) > 30:
            md.append(f"... and {len(failures) - 30} more (see raw JSON).")
        md.append("")

    md.append("## How this is generated")
    md.append("")
    md.append(
        "**mongo-ruby-driver's tests are run unmodified, against a "
        "standalone SecantusDB daemon.** The submodule at "
        "`vendor/mongo-ruby-driver/` is checked out at the pinned upstream "
        "tag with zero local edits. `ruby_validation/runner.py` runs "
        "`bundle install` (one-time per checkout), then does a two-phase "
        "spawn: phase 1 boots `python -m secantus --port 27018 "
        "--storage-path <tempdir>` without `--auth` and uses pymongo to "
        "createUser `root-user` (root role) and `ruby-test-user` "
        "(readWriteAnyDatabase + dbAdminAnyDatabase); phase 2 stops that "
        "daemon and restarts on the same tempdir **with `--auth`**, so "
        "the user records persist and the server now enforces auth. "
        "rspec runs against phase 2 with `MONGODB_URI=mongodb://"
        "root-user:password@127.0.0.1:27018/?authSource=admin`. "
        "On-disk tempdir is rmtree'd after the run."
    )
    md.append("")
    md.append(
        "These are **integration specs** — every test opens a real TCP "
        "connection to the SecantusDB daemon, SCRAM-authenticates, and "
        "exchanges wire commands end-to-end. The pass rate is therefore a "
        "true measure of SecantusDB's compatibility with the Ruby driver, "
        "not of the driver's own pure-code logic."
    )
    md.append("")
    md.append(
        "The include set is currently narrow on purpose: a single broken "
        "test in `spec/mongo/cursor_spec.rb` or `spec/mongo/bulk_write_spec.rb` "
        "can pin the runner indefinitely on a tailable getMore that never "
        "completes. Each new file is added to `include_paths.py` only "
        "after a manual confirmation that it terminates within the "
        "runner's wall-clock guard (300 s by default)."
    )
    md.append("")
    md.append(
        "Pending tests are honest skips driven by mutually-exclusive "
        "environment gates upstream tests opt into: `require_no_auth` "
        "(skips when auth is configured — and our gauge always runs "
        "with auth), `require_topology :single` (skips on replica-set "
        "topology — we always advertise as a single-node RS primary so "
        "change streams work), `min_server_version 4.5` (skips on >= "
        "4.6 — we report 7.0.0). Resolving them would require running "
        "additional gauge configurations rather than fixing SecantusDB "
        "behaviour. The pending number is therefore a lower bound on "
        "tests SecantusDB \"could pass\" if the runner spun up "
        "alternate daemons; the failed number is the only signal that "
        "matters for conformance."
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
