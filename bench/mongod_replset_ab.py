"""mongod standalone vs single-node replica set — the oplog-tax A/B.

Quantifies what mongod itself pays for the oplog on the same workload the
SecantusDB concurrency harness uses (8 KiB docs, ``insert_many`` batch 100,
per-writer collections). Three arms:

- ``standalone`` — no ``--replSet``: keeps no oplog at all. This is the mongod
  every other benchmark in this repo spawns.
- ``replset`` — single-node ``--replSet`` with the *implicit default* write
  concern. Since MongoDB 5.0 that default is ``w:majority``, and on a one-node
  set a majority ack requires a journal fsync — so this arm pays the oplog
  double-write AND an fsync per acknowledged batch.
- ``replset-w1`` — same server, explicit ``w=1&journal=false``: the oplog
  double-write with no fsync wait. This isolates the pure oplog tax and is the
  closest semantic match to SecantusDB's synchronous oplog
  (``transaction_sync=off``).

Measured 2026-07-28 (medians of 3 interleaved reps, quiesced box — see
``tasks/rust-perf-findings.md`` Finding 10 for the analysis):

======================  ==========  ==========
arm                     1 writer    8 writers
======================  ==========  ==========
standalone              113.2k/s    503k/s
replset-w1              84.0k/s     305k/s
replset (default WC)    11.8k/s     68.6k/s
======================  ==========  ==========
"""

from __future__ import annotations

import argparse
import os
import shutil
import statistics
import subprocess
import sys
import tempfile
import time

import pymongo

from bench.concurrency import _free_port, _wait_listen, run_writers

ARMS: tuple[tuple[str, bool, str], ...] = (
    ("standalone", False, ""),
    ("replset", True, ""),
    ("replset-w1", True, "?w=1&journal=false"),
)


def _mongod() -> str:
    found = shutil.which("mongod")
    if found:
        return found
    for cand in ("/opt/homebrew/bin/mongod",):
        if os.path.exists(cand):
            return cand
    raise SystemExit("mongod not found on PATH")


def wait_quiesce(limit: float, timeout: float = 240.0) -> None:
    t0 = time.monotonic()
    while os.getloadavg()[0] > limit and time.monotonic() - t0 < timeout:
        print(f"  load {os.getloadavg()[0]:.2f} > {limit}, waiting...", flush=True)
        time.sleep(15)


def spawn(port: int, dbpath: str, replset: bool) -> subprocess.Popen[bytes]:
    argv = [
        _mongod(),
        "--bind_ip",
        "127.0.0.1",
        "--port",
        str(port),
        "--dbpath",
        dbpath,
        "--quiet",
    ]
    if replset:
        argv += ["--replSet", "rs0"]
    return subprocess.Popen(
        argv,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.STDOUT,
    )


def init_replset(port: int) -> None:
    c = pymongo.MongoClient(
        "127.0.0.1", port, directConnection=True, serverSelectionTimeoutMS=20000
    )
    c.admin.command(
        "replSetInitiate",
        {"_id": "rs0", "members": [{"_id": 0, "host": f"127.0.0.1:{port}"}]},
    )
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        if c.admin.command("hello").get("isWritablePrimary"):
            c.close()
            return
        time.sleep(0.2)
    raise SystemExit("replset never elected a primary")


def run_arm(
    replset: bool, uri_opts: str, writers: list[int], duration: float, batch: int
) -> dict[int, float]:
    port = _free_port()
    dbpath = tempfile.mkdtemp(prefix="mongod-ab-")
    proc = spawn(port, dbpath, replset)
    try:
        if not _wait_listen("127.0.0.1", port, timeout=30):
            raise SystemExit("mongod didn't come up")
        if replset:
            init_replset(port)
        uri = f"mongodb://127.0.0.1:{port}/{uri_opts}"
        rates: dict[int, float] = {}
        for n in writers:
            stats, elapsed = run_writers(
                uri,
                n=n,
                duration=duration,
                batch=batch,
                db="harness",
                collection_prefix="inserts_8k_w",
                shared_collection=False,
            )
            total = sum(s[1] for s in stats if s)
            rates[n] = total / elapsed if elapsed else 0.0
        return rates
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=15)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()
        shutil.rmtree(dbpath, ignore_errors=True)


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    p = argparse.ArgumentParser(
        prog="mongod_replset_ab",
        description="mongod standalone vs single-node replica set write A/B.",
    )
    p.add_argument(
        "--duration",
        type=float,
        default=15.0,
        help="Seconds per (arm, writer-count) run (default: 15).",
    )
    p.add_argument(
        "--reps", type=int, default=3, help="Interleaved repetitions per arm (default: 3)."
    )
    p.add_argument("--writers", default="1,8", help="Comma-separated writer counts (default: 1,8).")
    p.add_argument(
        "--batch-size", type=int, default=100, help="Documents per insert_many (default: 100)."
    )
    p.add_argument(
        "--load-limit",
        type=float,
        default=5.0,
        help="Wait for 1-min load average below this before each run.",
    )
    return p.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    writers = [int(x) for x in args.writers.split(",") if x.strip()]
    results: dict[str, dict[int, list[float]]] = {a[0]: {} for a in ARMS}
    for rep in range(args.reps):
        for name, replset, uri_opts in ARMS:
            wait_quiesce(args.load_limit)
            rates = run_arm(replset, uri_opts, writers, args.duration, args.batch_size)
            for w, r in rates.items():
                results[name].setdefault(w, []).append(r)
            pretty = ", ".join(f"{w}w={r:,.0f}" for w, r in sorted(rates.items()))
            print(f"rep{rep} {name:<11} {pretty}", flush=True)
    print("\n=== medians (docs/s) ===")
    for name, per_w in results.items():
        row = ", ".join(f"{w}w={statistics.median(v):,.0f}" for w, v in sorted(per_w.items()))
        print(f"{name:<11} {row}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
