"""Turn a pytest-json-report JSON file into docs/validation-report.md.

Usage:
    python -m pymongo_validation.generate_report [--server python|rust] <raw.json> <output.md>

Groups tests by their first-level path component under
`vendor/pymongo-tests/test/` (e.g. "crud", "test_collection.py",
"bson_corpus") and emits a markdown table with passed / failed /
skipped / total / pass-rate columns plus a section listing the top
failures for triage.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
from collections import defaultdict
from pathlib import Path

import secantus

VENDOR_PREFIX = "vendor/pymongo-tests/test/"


def _category_for(nodeid: str) -> str:
    """First path component under vendor/pymongo-tests/test/ — directory or file."""
    rel = nodeid
    if VENDOR_PREFIX in rel:
        rel = rel.split(VENDOR_PREFIX, 1)[1]
    head = rel.split("/", 1)[0]
    return head.split("::", 1)[0]  # strip ::TestClass::test_method


def _read_pymongo_version() -> str:
    """Best-effort read of the pinned pymongo tag from the submodule."""
    head_file = Path("vendor/pymongo-tests/.git")
    # Submodule .git is a file pointing at the parent's modules/ dir.
    try:
        if head_file.is_file():
            modules_dir = Path(head_file.read_text().strip().removeprefix("gitdir: "))
            head = (Path("vendor/pymongo-tests") / modules_dir / "HEAD").resolve()
            if head.exists():
                return head.read_text().strip()
    except Exception:
        pass
    return "unknown"


def render(raw: dict, out_path: Path, *, server: str = "python") -> None:
    by_cat: dict[str, dict[str, int]] = defaultdict(
        lambda: {"passed": 0, "failed": 0, "skipped": 0, "errored": 0}
    )

    for test in raw.get("tests", []):
        cat = _category_for(test["nodeid"])
        outcome = test.get("outcome", "unknown")
        # ``pytest-subtests`` reports the parent test with outcome
        # ``"subtests passed"`` when its subtests all succeed (the
        # individual subtests don't appear as separate rows in the
        # JSON report). Treat that as a regular pass — historically
        # we bucketed it as ``errored`` via the dict's default branch,
        # which inflated the error count and made the gauge look
        # worse than reality.
        bucket = {
            "passed": "passed",
            "subtests passed": "passed",
            "failed": "failed",
            "skipped": "skipped",
            "error": "errored",
        }.get(outcome, "errored")
        by_cat[cat][bucket] += 1

    # Per-category rows.
    rows: list[tuple[str, int, int, int, int, int, str]] = []
    totals = {"passed": 0, "failed": 0, "skipped": 0, "errored": 0}
    for cat in sorted(by_cat):
        b = by_cat[cat]
        total = b["passed"] + b["failed"] + b["skipped"] + b["errored"]
        ran = b["passed"] + b["failed"] + b["errored"]
        rate = f"{(b['passed'] / ran * 100):.1f}%" if ran else "—"
        rows.append((cat, b["passed"], b["failed"], b["errored"], b["skipped"], total, rate))
        for k in totals:
            totals[k] += b[k]

    grand_total = sum(totals.values())
    grand_ran = totals["passed"] + totals["failed"] + totals["errored"]
    grand_rate = f"{(totals['passed'] / grand_ran * 100):.1f}%" if grand_ran else "—"

    # Top failures for triage.
    fails = [t for t in raw.get("tests", []) if t.get("outcome") in ("failed", "error")]
    fails.sort(key=lambda t: t["nodeid"])

    rust = server == "rust"
    md: list[str] = []
    md.append(
        "# pymongo Validation Report (Rust server)" if rust else "# pymongo Validation Report"
    )
    md.append("")
    md.append(
        f"Generated {dt.date.today().isoformat()} — SecantusDB "
        f"{secantus.__version__} vs pymongo {_read_pymongo_version()[:12]}"
        f" (`vendor/pymongo-tests/`)."
    )
    md.append("")
    if rust:
        md.append(
            "Run `uv run python -m invoke validate --server rust` to refresh. "
            "This is the R8 conformance gate from `tasks/rust-server-plan.md`: "
            "the same unmodified pymongo suite the headline gauge runs, pointed "
            "at the **Rust server** instead of the pure-Python one. The gap "
            "between this pass rate and `docs/validation-report.md` is the "
            "Rust server's remaining to-do list."
        )
    else:
        md.append(
            "Run `uv run python -m invoke validate` to refresh. The pass rate is the "
            "best honest measure of how close SecantusDB is to a complete MongoDB "
            "surrogate for the in-scope wire-protocol surface; gaps are the to-do list."
        )
    md.append("")
    md.append("## Summary by category")
    md.append("")
    md.append("| Category | Passed | Failed | Errored | Skipped | Total | Pass rate |")
    md.append("|---|---:|---:|---:|---:|---:|---:|")
    for cat, p, f, e, s, t, r in rows:
        md.append(f"| `{cat}` | {p} | {f} | {e} | {s} | {t} | {r} |")
    md.append(
        f"| **Overall** | **{totals['passed']}** | **{totals['failed']}** | "
        f"**{totals['errored']}** | **{totals['skipped']}** | "
        f"**{grand_total}** | **{grand_rate}** |"
    )
    md.append("")

    if fails:
        md.append(f"## Failures ({len(fails)})")
        md.append("")
        md.append("First 30 failure node-ids for manual triage:")
        md.append("")
        md.append("```")
        for t in fails[:30]:
            md.append(t["nodeid"])
        md.append("```")
        if len(fails) > 30:
            md.append(f"... and {len(fails) - 30} more (see raw JSON).")
        md.append("")

    md.append("## How this is generated")
    md.append("")
    if rust:
        server_clause = (
            "starts an embedded Rust server (`_secantus_server.RustServer("
            "storage_path=<fresh tempdir>, port=0)` — the in-process Rust "
            "accept loop over the pure-Rust engines and WiredTiger-backed "
            "storage; Python is only the launcher)"
        )
    else:
        server_clause = (
            "starts an embedded `SecantusDBServer(host='127.0.0.1', port=0, "
            "storage_path=<fresh tempdir>)`"
        )
    md.append(
        "**pymongo's tests are run unmodified.** The submodule at "
        "`vendor/pymongo-tests/` is checked out at the pinned upstream tag with "
        "zero local edits — `git diff HEAD` inside the submodule is empty. The "
        "integration is entirely external: `pymongo_validation/plugin.py` "
        f"{server_clause} (real on-disk WiredTiger via "
        "`tempfile.mkdtemp(prefix='secantus-pymongo-gauge-')`, not "
        "`:memory:`) in `pytest_configure` and writes the bound "
        "host/port into `DB_IP` + `DB_PORT` — the env vars pymongo's own "
        "`helpers_shared.py` reads at import time. Pytest then collects and "
        "runs the in-scope test paths defined in "
        "`pymongo_validation/include_paths.py`."
    )
    md.append("")
    md.append(
        "Tests gated on replica-set / sharding / auth / TLS / encryption "
        "topology self-skip — those skips are honest gaps, not failures. The "
        "pass rate above is therefore a meaningful conformance number: those "
        "are pymongo's actual tests, exercising SecantusDB the same way they "
        "exercise a real `mongod` in pymongo's CI."
    )
    md.append("")

    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text("\n".join(md))


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("raw_json", type=Path)
    parser.add_argument("output_md", type=Path)
    parser.add_argument(
        "--server",
        choices=["python", "rust"],
        default="python",
        help="Which SecantusDB server the gauge ran against (adjusts the report prose).",
    )
    args = parser.parse_args()
    raw = json.loads(args.raw_json.read_text())
    render(raw, args.output_md, server=args.server)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
