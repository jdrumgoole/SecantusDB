"""Oplog append-path experiment sweep (concurrency-parity program, PR 3).

Drives ``bench.concurrency --server rust`` once per (config, rep) with the
experiment env hooks set, interleaving configs within each rep so machine
drift cancels, and reports per-config medians plus the retention ratio
against the same-session no-oplog ceiling. Arms (see
``tasks/rust-perf-findings.md`` Finding 12 and the parity plan):

- ``baseline``  — today's sync default, no overrides.
- ``ceiling``   — ``SECANTUS_DISABLE_OPLOG=1`` (the no-oplog reference every
  other arm's retention is measured against).
- ``shards``    — ``SECANTUS_OPLOG_SHARDS`` 1/2/4/8 (16 = baseline): is
  rightmost-page contention still real post-RecordId, or is 16-way sharding
  now pure read-side overhead?
- ``tablecfg``  — ``SECANTUS_OPLOG_TABLE_EXTRA``: compressor off, bigger
  in-memory pages, append-friendly split behaviour on the oplog btrees.
- ``conncfg``   — ``SECANTUS_WT_CONFIG_EXTRA``: log prealloc + bigger cache,
  re-tested under sync (Finding 6 measured them mostly under async).
- ``datanolog`` — ``SECANTUS_DATA_NONLOGGED=1``: the structural probe of the
  mongod architecture (journal ONLY the oplog). CRASH-UNSAFE, measure-only;
  decides whether Phase A' (replay-on-open recovery) is worth building.

Usage:
    uv run python -m bench.oplog_sweep --arms baseline,ceiling,datanolog \
        --writers 1,8 --duration 12 --reps 2
"""

from __future__ import annotations

import argparse
import re
import statistics
import subprocess
import sys
import time
from pathlib import Path

#: (label, env overrides) per arm, in sweep order.
ARM_CONFIGS: dict[str, list[tuple[str, dict[str, str]]]] = {
    "baseline": [("sync-default", {})],
    "ceiling": [("no-oplog", {"SECANTUS_DISABLE_OPLOG": "1"})],
    "shards": [
        (f"shards-{n}", {"SECANTUS_OPLOG_SHARDS": str(n)}) for n in (1, 2, 4, 8)
    ],
    "tablecfg": [
        ("oplog-nocompress", {"SECANTUS_OPLOG_TABLE_EXTRA": "block_compressor=none"}),
        ("oplog-bigpage", {"SECANTUS_OPLOG_TABLE_EXTRA": "memory_page_max=10MB"}),
        (
            "oplog-append-split",
            {"SECANTUS_OPLOG_TABLE_EXTRA": "split_pct=100,leaf_page_max=128KB"},
        ),
    ],
    "conncfg": [
        (
            "log-prealloc",
            {"SECANTUS_WT_CONFIG_EXTRA": "log=(enabled=true,file_max=512MB,prealloc=true)"},
        ),
        ("cache-4g", {"SECANTUS_WT_CONFIG_EXTRA": "cache_size=4G"}),
    ],
    "datanolog": [("data-nonlogged", {"SECANTUS_DATA_NONLOGGED": "1"})],
}

_ROW = re.compile(r"^(\d+)\s+[\d,]+\s+[\d.]+s\s+([\d,]+)", re.MULTILINE)


def run_config(
    label: str, env: dict[str, str], writers: str, duration: int, batch_size: int
) -> dict[int, int]:
    """One bench.concurrency run; returns {writer_count: docs_per_sec}."""
    import os

    cmd = [
        sys.executable,
        "-m",
        "bench.concurrency",
        "--server",
        "rust",
        "--writers",
        writers,
        "--duration",
        str(duration),
        "--batch-size",
        str(batch_size),
    ]
    proc = subprocess.run(
        cmd,
        capture_output=True,
        text=True,
        env={**os.environ, **env},
        check=False,
    )
    if proc.returncode != 0:
        print(f"  !! {label}: bench.concurrency failed:\n{proc.stdout[-2000:]}{proc.stderr[-2000:]}")
        return {}
    out: dict[int, int] = {}
    for m in _ROW.finditer(proc.stdout):
        out[int(m.group(1))] = int(m.group(2).replace(",", ""))
    return out


def wait_for_quiet(load_limit: float) -> None:
    while True:
        load1 = float(Path("/proc/loadavg").read_text().split()[0]) if Path(
            "/proc/loadavg"
        ).exists() else _mac_load1()
        if load1 < load_limit:
            return
        print(f"  load {load1:.2f} > {load_limit}, waiting...", flush=True)
        time.sleep(15)


def _mac_load1() -> float:
    out = subprocess.run(["sysctl", "-n", "vm.loadavg"], capture_output=True, text=True)
    return float(out.stdout.split()[1])


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--arms", default="baseline,ceiling,shards,tablecfg,conncfg,datanolog")
    ap.add_argument("--writers", default="1,8")
    ap.add_argument("--duration", type=int, default=12)
    ap.add_argument("--reps", type=int, default=2)
    ap.add_argument("--batch-size", type=int, default=100)
    ap.add_argument("--load-limit", type=float, default=4.0)
    args = ap.parse_args()

    configs: list[tuple[str, dict[str, str]]] = []
    for arm in args.arms.split(","):
        try:
            configs.extend(ARM_CONFIGS[arm.strip()])
        except KeyError:
            ap.error(f"unknown arm {arm!r} (choose from {', '.join(ARM_CONFIGS)})")

    results: dict[str, dict[int, list[int]]] = {label: {} for label, _ in configs}
    for rep in range(1, args.reps + 1):
        for label, env in configs:
            wait_for_quiet(args.load_limit)
            print(f"rep{rep} {label} ...", flush=True)
            for w, rate in run_config(
                label, env, args.writers, args.duration, args.batch_size
            ).items():
                results[label].setdefault(w, []).append(rate)
                print(f"  {w}w: {rate:,} docs/s", flush=True)

    writer_counts = sorted({w for r in results.values() for w in r})
    ceiling = {
        w: statistics.median(results["no-oplog"][w])
        for w in writer_counts
        if "no-oplog" in results and results.get("no-oplog", {}).get(w)
    }
    print("\n=== medians (docs/s; retention vs same-session no-oplog ceiling) ===")
    header = "config".ljust(20) + "".join(f"{w}w".rjust(22) for w in writer_counts)
    print(header)
    for label, per_w in results.items():
        cells = []
        for w in writer_counts:
            if not per_w.get(w):
                cells.append("-".rjust(22))
                continue
            med = statistics.median(per_w[w])
            ret = f" ({med / ceiling[w] * 100:.0f}%)" if ceiling.get(w) else ""
            cells.append(f"{med:,.0f}{ret}".rjust(22))
        print(label.ljust(20) + "".join(cells))


if __name__ == "__main__":
    main()
