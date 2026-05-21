"""Render the raw ``rust-raw.json`` into ``docs/validation-report-rust.md``.

Same shape as ``pymongo_validation/generate_report.py`` and
``ruby_validation/generate_report.py``: a per-module table of
passed / failed / ignored counts, an overall row, and a
truncated failures list for triage.

Usage::

    uv run python -m rust_validation.generate_report \\
        .validation/rust-raw.json docs/validation-report-rust.md
"""

from __future__ import annotations

import datetime as _dt
import json
import subprocess
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
VENDOR = REPO_ROOT / "vendor" / "mongo-rust-driver"


def _vendor_ref() -> str:
    """Short driver ref for the report header (commit hash, falling
    back to a 'detached' marker)."""
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
    """Read SecantusDB's version from ``src/secantus/__init__.py`` so
    the report self-stamps the version that produced it."""
    init = REPO_ROOT / "src" / "secantus" / "__init__.py"
    for line in init.read_text().splitlines():
        if line.startswith("__version__"):
            return line.split("=", 1)[1].strip().strip('"').strip("'")
    return "unknown"


def _module_of(test_name: str) -> str:
    """Extract the module the test lives in (the slice between
    ``test::`` and the final ``::<test_name>``). ``test::client::foo``
    → ``client``; ``test::coll::bar`` → ``coll``."""
    parts = test_name.split("::")
    if len(parts) >= 3 and parts[0] == "test":
        return parts[1]
    return parts[0] if parts else "other"


def _render(raw: dict) -> str:
    tests = raw["tests"]
    by_module: dict[str, dict[str, int]] = {}
    for t in tests:
        mod = _module_of(t["name"])
        bucket = by_module.setdefault(mod, {"passed": 0, "failed": 0, "ignored": 0})
        bucket[t["outcome"]] += 1
    overall = raw["summary"]
    total_overall = overall["passed"] + overall["failed"] + overall["ignored"]
    pass_rate_overall = 100.0 * overall["passed"] / max(1, overall["passed"] + overall["failed"])

    lines: list[str] = []
    lines.append("# mongo-rust-driver Validation Report")
    lines.append("")
    lines.append(
        f"Generated {_dt.date.today().isoformat()} — "
        f"SecantusDB {_secantus_version()} vs mongo-rust-driver "
        f"{_vendor_ref()} (`vendor/mongo-rust-driver/`)."
    )
    lines.append("")
    lines.append(
        "Run `uv run python -m invoke validate-rust` to refresh. The "
        "Rust-driver analogue of the pymongo / mongo-go-driver / "
        "mongo-node-driver / mongo-java-driver / mongo-ruby-driver "
        "gauges — the language MongoDB consumers reach for when they "
        "want native performance + async."
    )
    lines.append("")
    lines.append("## Summary")
    lines.append("")
    lines.append("| Module | Passed | Failed | Ignored | Total | Pass rate |")
    lines.append("|---|---:|---:|---:|---:|---:|")
    for mod in sorted(by_module):
        b = by_module[mod]
        total = b["passed"] + b["failed"] + b["ignored"]
        denom = b["passed"] + b["failed"]
        rate = 100.0 * b["passed"] / max(1, denom) if denom else 100.0
        lines.append(
            f"| `{mod}` | {b['passed']} | {b['failed']} | {b['ignored']} | {total} | {rate:.1f}% |"
        )
    lines.append(
        f"| **Overall** | **{overall['passed']}** | "
        f"**{overall['failed']}** | **{overall['ignored']}** | "
        f"**{total_overall}** | **{pass_rate_overall:.1f}%** |"
    )
    lines.append("")
    if overall["failed"]:
        lines.append(f"## Failures ({overall['failed']})")
        lines.append("")
        lines.append("First 30 failed tests for triage:")
        lines.append("")
        lines.append("```")
        for f in raw.get("failures", [])[:30]:
            msg = f.get("message", "").strip()
            lines.append(f"{f['name']}")
            if msg:
                lines.append(f"    {msg}")
        lines.append("```")
        lines.append("")
    lines.append("## How this is generated")
    lines.append("")
    lines.append(
        "``invoke validate-rust`` spawns a SecantusDB daemon on a "
        "fresh ephemeral port, runs ``cargo test --lib -p mongodb`` "
        "against the curated include set with ``MONGODB_URI`` "
        "explicitly overridden in the subprocess env (so the user's "
        "ambient env can't leak through to a real mongod), parses "
        "cargo's per-test output, and writes this report. The list "
        "of in-scope tests lives in "
        "``rust_validation/include_paths.py``."
    )
    lines.append("")
    return "\n".join(lines)


def main(argv: list[str]) -> int:
    if len(argv) != 3:
        print("Usage: generate_report.py <raw.json> <out.md>", file=sys.stderr)
        return 2
    raw = json.loads(Path(argv[1]).read_text())
    out = Path(argv[2])
    out.write_text(_render(raw))
    print(f"Wrote {out}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
