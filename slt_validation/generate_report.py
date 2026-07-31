"""Turn the sqllogictest gauge's raw JSON into docs/validation-report-slt.md.

Usage:
    python -m slt_validation.generate_report <raw.json> <output.md>

Per-file PASS / FAIL / EXPECTED-DIVERGENCE table plus the first error of each
unexpected failure. A file in ``EXPECTED_DIVERGENCES`` that *passes* is
flagged loudly — the divergence resolved and belongs back in plain INCLUDE.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import re
from pathlib import Path

from .include_paths import EXPECTED_DIVERGENCES


def _secantus_version() -> str:
    try:
        import secantus

        return secantus.__version__
    except ImportError:
        init = Path(__file__).resolve().parent.parent / "src" / "secantus" / "__init__.py"
        m = re.search(r'__version__ = "([^"]+)"', init.read_text())
        return m.group(1) if m else "unknown"


def _corpus_rev() -> str:
    import subprocess

    try:
        return subprocess.run(
            ["git", "-C", "vendor/sqllogictest", "rev-parse", "--short=12", "HEAD"],
            capture_output=True,
            text=True,
            check=True,
            timeout=10,
        ).stdout.strip()
    except (OSError, subprocess.SubprocessError):
        return "unknown"


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("raw", type=Path)
    ap.add_argument("out", type=Path)
    args = ap.parse_args()
    results: dict[str, dict] = json.loads(args.raw.read_text())

    def _file_of(key: str) -> str:
        # Two-lane keys are ``<engine>:<file>``; legacy raw files are bare.
        return key.split(":", 1)[1] if ":" in key else key

    passed = [f for f, r in results.items() if r["ok"]]
    failed = [f for f, r in results.items() if not r["ok"]]
    expected = [f for f in failed if _file_of(f) in EXPECTED_DIVERGENCES]
    unexpected = [f for f in failed if _file_of(f) not in EXPECTED_DIVERGENCES]
    resolved = sorted({_file_of(f) for f in passed if _file_of(f) in EXPECTED_DIVERGENCES})

    lines: list[str] = []
    lines.append("# sqllogictest conformance report")
    lines.append("")
    lines.append(
        f"SecantusDB (Python server) {_secantus_version()} · corpus `gregrahn/sqllogictest` "
        f"@ `{_corpus_rev()}` · sqllogictest-rs over pgwire · "
        f"{dt.date.today().isoformat()}"
    )
    lines.append("")
    lines.append(
        f"**{len(passed)}/{len(results)} files pass end-to-end** "
        f"({len(expected)} expected divergences, {len(unexpected)} unexpected failures)."
    )
    lines.append("")
    lines.append("Regenerate with `uv run python -m invoke validate-slt`.")
    lines.append("")
    lines.append("| lane | file | result | seconds |")
    lines.append("|---|---|---|---:|")
    for f, r in results.items():
        if r["ok"]:
            status = "pass"
        elif _file_of(f) in EXPECTED_DIVERGENCES:
            status = "expected divergence"
        else:
            status = "**FAIL**"
        lane = r.get("engine", "postgres")
        lines.append(f"| {lane} | `{_file_of(f)}` | {status} | {r['seconds']} |")
    lines.append("")

    if resolved:
        lines.append("## Resolved divergences — move back to plain INCLUDE")
        lines.append("")
        for f in resolved:
            lines.append(
                f"- `{f}` passes in at least one lane; check both before dropping its "
                "`EXPECTED_DIVERGENCES` entry."
            )
        lines.append("")

    if expected:
        lines.append("## Expected divergences")
        lines.append("")
        for f in expected:
            lines.append(f"- `{f}` — {EXPECTED_DIVERGENCES[_file_of(f)]}")
        lines.append("")

    if unexpected:
        lines.append("## Unexpected failures")
        lines.append("")
        for f in unexpected:
            lines.append(f"### `{f}`")
            lines.append("")
            lines.append("```")
            lines.append(results[f]["error"])
            lines.append("```")
            lines.append("")

    args.out.write_text("\n".join(lines))
    print(f"Wrote {args.out}")
    return 1 if unexpected else 0


if __name__ == "__main__":
    raise SystemExit(main())
