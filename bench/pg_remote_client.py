#!/usr/bin/env python3
"""One client machine's share of a `pg_concurrency` trial, against a server
somewhere else.

`bench/pg_concurrency.py` runs its clients beside the server. This runs them
on a machine of their own: `bench/do_pg_bench.py` starts it on each client
droplet at once, and adds up what they report. It reuses `pg_concurrency`'s
worker, so a statement here is the statement there.

Every client process waits for `--start-at` (a Unix time) before it begins,
so two machines' windows are the same window. It prints one line of JSON:
the statements committed, the seconds, and how busy this machine's CPUs were,
which says whether the clients or the server were the limit.
"""

from __future__ import annotations

import argparse
import json
import multiprocessing as mp
import os
import sys
import time
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
if str(REPO) not in sys.path:
    sys.path.insert(0, str(REPO))


def _cpu() -> tuple[int, int]:
    """(busy, total) jiffies since boot, from /proc/stat."""
    fields = [int(x) for x in Path("/proc/stat").read_text().splitlines()[0].split()[1:9]]
    idle = fields[3] + fields[4]
    return sum(fields) - idle, sum(fields)


def _wait_then_work(start_at: float, *args: object) -> None:
    from bench.pg_concurrency import _worker

    time.sleep(max(0.0, start_at - time.time()))
    _worker(*args)


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--dsn", required=True)
    ap.add_argument("--tables", required=True, help="comma-separated; client i uses table i")
    ap.add_argument("--workload", choices=["insert", "update", "select"], required=True)
    ap.add_argument("--clients", type=int, required=True)
    ap.add_argument("--seconds", type=float, default=10.0)
    ap.add_argument("--start-at", type=float, required=True, help="Unix time to begin at")
    ap.add_argument("--slot-base", type=int, default=0, help="first seeded row this machine uses")
    args = ap.parse_args(argv)

    from bench.pg_concurrency import _SEEDED_ROWS

    tables = args.tables.split(",")
    q: mp.Queue = mp.Queue()
    procs = [
        mp.Process(
            target=_wait_then_work,
            args=(
                args.start_at,
                args.dsn,
                tables[i % len(tables)],
                args.workload,
                args.seconds,
                q,
                (args.slot_base + i) % _SEEDED_ROWS,
            ),
        )
        for i in range(args.clients)
    ]
    for p in procs:
        p.start()
    time.sleep(max(0.0, args.start_at - time.time()))
    busy0, total0 = _cpu()
    committed, errors = 0, []
    for _ in procs:
        kind, payload = q.get()
        if kind == "error":
            errors.append(payload)
        else:
            committed += int(payload)
    busy1, total1 = _cpu()
    for p in procs:
        p.join()
    print(
        json.dumps(
            {
                "committed": committed,
                "seconds": args.seconds,
                "errors": errors,
                "cpu_busy_pct": round(100.0 * (busy1 - busy0) / max(1, total1 - total0), 1),
                "cpus": os.cpu_count(),
            }
        )
    )
    return 1 if errors else 0


if __name__ == "__main__":
    raise SystemExit(main())
