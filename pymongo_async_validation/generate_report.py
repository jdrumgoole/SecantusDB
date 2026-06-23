"""Turn a pytest-json-report JSON file into docs/validation-report-pymongo-async.md.

Usage:
    python -m pymongo_async_validation.generate_report \\
        [--server python|rust] <raw.json> <output.md>

The async analogue of ``pymongo_validation.generate_report``. Groups
tests by their file name under ``vendor/pymongo-tests/test/asynchronous/``
(e.g. "test_collection.py", "test_change_stream.py") and emits a markdown
table with passed / failed / errored / skipped / total / pass-rate columns
plus a section listing the top failures for triage.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
from collections import defaultdict
from pathlib import Path

import secantus

VENDOR_PREFIX = "vendor/pymongo-tests/test/asynchronous/"


def _category_for(nodeid: str) -> str:
    """File name under vendor/pymongo-tests/test/asynchronous/."""
    rel = nodeid
    if VENDOR_PREFIX in rel:
        rel = rel.split(VENDOR_PREFIX, 1)[1]
    head = rel.split("/", 1)[0]
    return head.split("::", 1)[0]  # strip ::TestClass::test_method


def _read_pymongo_version() -> str:
    """Best-effort read of the pinned pymongo tag from the submodule."""
    head_file = Path("vendor/pymongo-tests/.git")
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
        bucket = {
            "passed": "passed",
            "subtests passed": "passed",
            "failed": "failed",
            "skipped": "skipped",
            "error": "errored",
        }.get(outcome, "errored")
        by_cat[cat][bucket] += 1

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

    fails = [t for t in raw.get("tests", []) if t.get("outcome") in ("failed", "error")]
    fails.sort(key=lambda t: t["nodeid"])

    rust = server == "rust"
    md: list[str] = []
    md.append(
        "# pymongo async Validation Report (Rust server)"
        if rust
        else "# pymongo async Validation Report"
    )
    md.append("")
    md.append(
        f"Generated {dt.date.today().isoformat()} — SecantusDB "
        f"{secantus.__version__} vs pymongo {_read_pymongo_version()[:12]}"
        f" (`vendor/pymongo-tests/test/asynchronous/`)."
    )
    md.append("")
    if rust:
        md.append(
            "Run `uv run python -m invoke validate-pymongo-async --server rust` to "
            "refresh. This is the async-driver analogue of the R8 conformance gate: "
            "pymongo's native `AsyncMongoClient` suite pointed at the **Rust server**."
        )
    else:
        md.append(
            "Run `uv run python -m invoke validate-pymongo-async` to refresh. This is "
            "the async sibling of the headline pymongo gauge: it drives pymongo's "
            "native `AsyncMongoClient` API (the async/await wire path that replaced "
            "Motor) over the same in-scope CRUD / cursor / change-stream / "
            "command-monitoring surface. A gap versus `docs/validation-report.md` "
            "means the async code path exercises something the sync path doesn't."
        )
    md.append("")
    md.append("## Summary by test file")
    md.append("")
    md.append("| Test file | Passed | Failed | Errored | Skipped | Total | Pass rate |")
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
            "storage_path=<fresh tempdir>, port=0)`)"
        )
    else:
        server_clause = (
            "starts an embedded `SecantusDBServer(host='127.0.0.1', port=0, "
            "storage_path=<fresh tempdir>)`"
        )
    md.append(
        "**pymongo's async tests are run unmodified.** The submodule at "
        "`vendor/pymongo-tests/` is checked out at the pinned upstream tag with "
        "zero local edits. The integration is entirely external: the shared "
        "`pymongo_validation/plugin.py` "
        f"{server_clause} (real on-disk WiredTiger) before pymongo's conftest is "
        "imported and writes the bound host/port into `DB_IP` + `DB_PORT` — the "
        "env vars pymongo's `helpers_shared.py` reads at import time, which the "
        "async `AsyncClientContext` also resolves from. Pytest then runs the "
        "in-scope paths from `pymongo_async_validation/include_paths.py` under "
        "`pytest-asyncio` (`asyncio_mode=auto`)."
    )
    md.append("")
    md.append(
        "Tests gated on replica-set / sharding / auth / TLS / encryption topology "
        "self-skip — those skips are honest gaps, not failures. The pass rate is a "
        "meaningful conformance number for SecantusDB's behaviour under pymongo's "
        "async driver, exercised the same way pymongo's own CI exercises a real "
        "`mongod`."
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
