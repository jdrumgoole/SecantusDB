"""Rewrite the published three-droplet head-to-head table from run artifacts.

`docs/benchmark.md`'s "Over a real network, against a real MongoDB" section was
the last benchmark surface a human had to update by hand: the per-operation and
concurrency tables are rewritten by `latency_chart` / `concurrency_chart`, but
this one was copied out of `release-benchmark`'s output manually.

It went stale twice. After the lz4 switch the page carried a post-lz4 droplet
section above a pre-lz4 latency table; and across 0.6.0b15/b16 it kept showing
`0.5.3-beta.161` at 11,099 ops/s against mongod 8.0.29 while the header directly
above it named 8.0.31 — figures quoted in two release summaries were never
actually published. Both times the cause was the same: a step that depends on
someone remembering.

So this reads `bench/results/do/<run>/comparison.md` — the artifact
`release-benchmark` already writes — and rewrites the marked region. Pair it
with `tests/test_benchmark_table_fresh.py`, which fails when the published table
does not match the newest run.

Usage:
    uv run python -m bench.head_to_head_chart            # newest run
    uv run python -m bench.head_to_head_chart --run-dir bench/results/do/<stamp>
    uv run python -m bench.head_to_head_chart --check    # exit 1 if stale
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
RUNS_DIR = REPO / "bench" / "results" / "do"
DOCS = REPO / "docs" / "benchmark.md"

TABLE_BEGIN = "<!-- head-to-head:begin -->"
TABLE_END = "<!-- head-to-head:end -->"


class RunData:
    """The fields the published paragraph and table need."""

    def __init__(self, text: str, run_id: str) -> None:
        self.run_id = run_id
        raw_server = self._field(text, "server")
        self.region = raw_server.split()[-1]
        self.server = raw_server
        self.clients = self._field(text, "clients")
        self.workload = self._field(text, "workload")
        self.cache = self._field(text, "cache")
        self.passes = self._field(text, "passes")
        self.rows = self._rows(text)
        self.ratios = self._ratios(text)

    @staticmethod
    def _field(text: str, name: str) -> str:
        m = re.search(rf"^{re.escape(name)}\s+(.+)$", text, re.MULTILINE)
        if not m:
            raise SystemExit(f"comparison.md: no '{name}' line")
        return m.group(1).strip()

    @staticmethod
    def _rows(text: str) -> dict[str, dict[str, str]]:
        out: dict[str, dict[str, str]] = {}
        pattern = re.compile(
            r"^\|\s*\*\*(secantusdb|mongod)\*\*\s*\|\s*(\S+)\s*\|\s*\*\*([\d,]+)\*\*\s*\|"
            r"\s*([\d.]+%)\s*\|\s*([\d.]+)\s*\|\s*([\d.]+)\s*\|\s*([\d.]+)\s*\|\s*([\d.]+%)\s*\|",
            re.MULTILINE,
        )
        for m in pattern.finditer(text):
            out[m.group(1)] = {
                "version": m.group(2).lstrip("v"),
                "ops": m.group(3),
                "spread": m.group(4),
                "p50": m.group(5),
                "p99": m.group(6),
                "p999": m.group(7),
                "cpu": m.group(8),
            }
        missing = {"secantusdb", "mongod"} - set(out)
        if missing:
            raise SystemExit(f"comparison.md: no summary row for {sorted(missing)}")
        return out

    @staticmethod
    def _ratios(text: str) -> dict[str, str]:
        out = {}
        for m in re.finditer(r"^\|\s*([\w. ()/]+?)\s*\|\s*([\d.]+)x\s*\|$", text, re.MULTILINE):
            out[m.group(1).strip()] = m.group(2)
        return out


def newest_run() -> Path:
    runs = sorted(p for p in RUNS_DIR.glob("*/") if (p / "comparison.md").is_file())
    if not runs:
        raise SystemExit(f"no run with a comparison.md under {RUNS_DIR}")
    return runs[-1]


def _commas(n: str) -> str:
    """9338 -> 9,338. The artifact writes bare integers; the page uses groups."""
    try:
        return f"{int(n.replace(',', '')):,}"
    except ValueError:
        return n


def _row(label: str, r: dict[str, str]) -> str:
    return (
        f"| {label} | {r['version']} | **{_commas(r['ops'])}** | {r['spread']} "
        f"| {r['p50']} ms | {r['p99']} ms | **{r['p999']} ms** | {r['cpu']} |"
    )


def render(run: RunData) -> str:
    s, m = run.rows["secantusdb"], run.rows["mongod"]
    date = f"{run.run_id[0:4]}-{run.run_id[4:6]}-{run.run_id[6:8]}"
    thr = run.ratios.get("throughput (ops/s)", "?")
    p50 = run.ratios.get("p50 latency", "?")
    p999 = run.ratios.get("p99.9 latency", "?")
    # The artifact's fields carry their own formatting -- the server line ends
    # with its region, the cache line annotates "(both engines)", the passes
    # line explains itself. Strip those so the sentence reads as prose.
    server = re.sub(r"\s+", " ", run.server).replace(f" {run.region}", "").strip()
    cache = run.cache.split("(")[0].strip()
    passes = run.passes.split("(")[0].strip()
    return "\n".join(
        [
            f"Measured {date} on DigitalOcean `{run.region}`: a `{server}` server and "
            f"{run.clients}, 8 KiB **incompressible**",
            f"documents, a 70/20/10 insert/find/update mix, a {cache} WiredTiger cache for",
            f"both engines, and {passes} interleaved passes:",
            "",
            "| engine | version | ops/s (median) | spread | p50 | p99 | p99.9 | server CPU |",
            "| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |",
            _row("SecantusDB", s),
            _row("mongod", m),
            "",
            f"**SecantusDB reaches {thr}x of MongoDB's throughput on this workload, with p50",
            f"latency within {p50}x and p99.9 within {p999}x.** Both engines saturated the same",
            "server while the clients sat idle, so both figures are server-bound and the",
            f"comparison is fair. Run-to-run spread was about {s['spread']}.",
        ]
    )


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--run-dir", type=Path, default=None, help="run directory (default: newest)")
    ap.add_argument(
        "--check",
        action="store_true",
        help="exit 1 if the published table does not match the run, without writing",
    )
    args = ap.parse_args(argv)

    run_dir = args.run_dir or newest_run()
    comparison = run_dir / "comparison.md"
    run = RunData(comparison.read_text(encoding="utf-8"), run_dir.name.rstrip("/"))
    block = render(run)

    original = DOCS.read_text(encoding="utf-8")
    pattern = re.compile(re.escape(TABLE_BEGIN) + r".*?" + re.escape(TABLE_END), re.DOTALL)
    if len(pattern.findall(original)) != 1:
        raise SystemExit(f"{DOCS}: expected exactly one {TABLE_BEGIN} ... {TABLE_END} region")
    updated = pattern.sub(lambda _m: f"{TABLE_BEGIN}\n{block}\n{TABLE_END}", original)

    if args.check:
        if updated != original:
            print(
                f"docs/benchmark.md's head-to-head table is stale against {run_dir.name}.\n"
                "Refresh it with:  uv run python -m bench.head_to_head_chart",
                file=sys.stderr,
            )
            return 1
        print(f"head-to-head table is current with {run_dir.name}")
        return 0

    if updated != original:
        DOCS.write_text(updated, encoding="utf-8", newline="\n")
        print(f"rewrote {DOCS} from {run_dir.name}")
    else:
        print(f"{DOCS} already current with {run_dir.name}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
