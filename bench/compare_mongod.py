"""Benchmark SecantusDB vs single-node mongod on identical workloads.

Both servers use the same WiredTiger storage engine (mongod ships it,
SecantusDB vendors the same C library), driven by the same pymongo
client over the wire protocol. The only differences in the hot path
are the command-dispatch / aggregation / planner layers above WT —
which is exactly what we want this benchmark to measure.

Spawns each server fresh on an OS-assigned port with its own tmp
data dir (both fully on-disk WiredTiger; mongod 8.x has no in-memory
storage engine in the community build, so we pick "both on disk" as
the fair baseline). Discards both data dirs at the end.

Operations compared:
  - bulk insert (10k docs)
  - find by indexed equality
  - find by indexed range
  - aggregation $group
  - update_many
  - delete_many

Each operation is timed N=5 times; we report median + p95 in
microseconds. Output is a markdown table written to
docs/benchmark.md.
"""

from __future__ import annotations

import argparse
import os
import shutil
import socket
import statistics
import subprocess
import sys
import tempfile
import time
from contextlib import contextmanager
from pathlib import Path
from typing import Callable, Iterator

from pymongo import MongoClient

REPO_ROOT = Path(__file__).resolve().parent.parent
N_ITERATIONS = 5
N_DOCS = 10_000


def _free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def _wait_for_listener(host: str, port: int, timeout: float = 30.0) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            with socket.create_connection((host, port), timeout=0.5):
                return
        except OSError:
            time.sleep(0.1)
    raise RuntimeError(f"{host}:{port} did not accept a connection within {timeout}s")


@contextmanager
def secantus_daemon(data_dir: Path) -> Iterator[str]:
    """Spawn SecantusDB on a free port; yield URI."""
    port = _free_port()
    proc = subprocess.Popen(
        [
            sys.executable, "-m", "secantus",
            "--host", "127.0.0.1", "--port", str(port),
            "--storage-path", str(data_dir),
            "--log-level", "WARNING",
        ],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    try:
        _wait_for_listener("127.0.0.1", port)
        yield f"mongodb://127.0.0.1:{port}"
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()


@contextmanager
def mongod_daemon(data_dir: Path) -> Iterator[str]:
    """Spawn mongod on a free port; yield URI."""
    port = _free_port()
    log = data_dir / "mongod.log"
    proc = subprocess.Popen(
        [
            "mongod",
            "--bind_ip", "127.0.0.1",
            "--port", str(port),
            "--dbpath", str(data_dir),
            "--logpath", str(log),
            "--noauth",
            "--quiet",
        ],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    try:
        _wait_for_listener("127.0.0.1", port)
        # Tiny additional grace: mongod accepts the TCP connection before
        # the wire-protocol handshake is fully ready.
        time.sleep(0.5)
        yield f"mongodb://127.0.0.1:{port}"
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()


# ----------------------------- workloads -----------------------------------

def _docs() -> list[dict]:
    return [
        {
            "_id": i,
            "name": f"item_{i}",
            "category": f"cat_{i % 20}",
            "value": i % 1000,
            "active": i % 2 == 0,
        }
        for i in range(N_DOCS)
    ]


def workload_insert_many(uri: str) -> float:
    client = MongoClient(uri, serverSelectionTimeoutMS=5000)
    try:
        coll = client["bench"]["items"]
        coll.drop()
        docs = _docs()
        t0 = time.perf_counter()
        coll.insert_many(docs, ordered=False)
        t1 = time.perf_counter()
        return t1 - t0
    finally:
        client.close()


def workload_find_eq_indexed(uri: str) -> float:
    client = MongoClient(uri, serverSelectionTimeoutMS=5000)
    try:
        coll = client["bench"]["items"]
        coll.drop()
        coll.insert_many(_docs(), ordered=False)
        coll.create_index([("category", 1)])
        t0 = time.perf_counter()
        # Touch each category once (~20 lookups, each returning ~500 docs).
        for c in range(20):
            list(coll.find({"category": f"cat_{c}"}))
        t1 = time.perf_counter()
        return t1 - t0
    finally:
        client.close()


def workload_find_range_indexed(uri: str) -> float:
    client = MongoClient(uri, serverSelectionTimeoutMS=5000)
    try:
        coll = client["bench"]["items"]
        coll.drop()
        coll.insert_many(_docs(), ordered=False)
        coll.create_index([("value", 1)])
        t0 = time.perf_counter()
        list(coll.find({"value": {"$gte": 200, "$lt": 800}}))
        t1 = time.perf_counter()
        return t1 - t0
    finally:
        client.close()


def workload_aggregate_group(uri: str) -> float:
    client = MongoClient(uri, serverSelectionTimeoutMS=5000)
    try:
        coll = client["bench"]["items"]
        coll.drop()
        coll.insert_many(_docs(), ordered=False)
        t0 = time.perf_counter()
        list(
            coll.aggregate(
                [
                    {"$group": {"_id": "$category", "count": {"$sum": 1}, "max_v": {"$max": "$value"}}},
                    {"$sort": {"_id": 1}},
                ]
            )
        )
        t1 = time.perf_counter()
        return t1 - t0
    finally:
        client.close()


def workload_update_many(uri: str) -> float:
    client = MongoClient(uri, serverSelectionTimeoutMS=5000)
    try:
        coll = client["bench"]["items"]
        coll.drop()
        coll.insert_many(_docs(), ordered=False)
        t0 = time.perf_counter()
        coll.update_many({"active": True}, {"$inc": {"value": 1}})
        t1 = time.perf_counter()
        return t1 - t0
    finally:
        client.close()


def workload_delete_many(uri: str) -> float:
    client = MongoClient(uri, serverSelectionTimeoutMS=5000)
    try:
        coll = client["bench"]["items"]
        coll.drop()
        coll.insert_many(_docs(), ordered=False)
        t0 = time.perf_counter()
        coll.delete_many({"value": {"$lt": 500}})
        t1 = time.perf_counter()
        return t1 - t0
    finally:
        client.close()


WORKLOADS: list[tuple[str, Callable[[str], float]]] = [
    ("insert_many (10k docs)", workload_insert_many),
    ("find indexed eq (20×500 docs)", workload_find_eq_indexed),
    ("find indexed range ($gte/$lt)", workload_find_range_indexed),
    ("aggregate $group + $sort", workload_aggregate_group),
    ("update_many ($inc on 5k docs)", workload_update_many),
    ("delete_many (5k docs by range)", workload_delete_many),
]


# ----------------------------- harness -------------------------------------

def time_workload(uri: str, workload: Callable[[str], float], n: int) -> tuple[float, float]:
    """Run `workload` `n` times against `uri`. Return (median, p95) seconds."""
    samples = [workload(uri) for _ in range(n)]
    samples.sort()
    median = statistics.median(samples)
    p95_idx = max(0, int(n * 0.95) - 1)
    p95 = samples[p95_idx]
    return median, p95


def fmt_ms(seconds: float) -> str:
    return f"{seconds * 1000:.1f}"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--out", type=Path, default=REPO_ROOT / "docs" / "benchmark.md")
    parser.add_argument("--iterations", type=int, default=N_ITERATIONS)
    args = parser.parse_args()

    if shutil.which("mongod") is None:
        print("mongod not on PATH; install MongoDB Community Server to run this bench.", file=sys.stderr)
        return 2

    results: list[tuple[str, float, float, float, float]] = []
    # (workload_name, secantus_med, secantus_p95, mongod_med, mongod_p95)

    print(f"Running {len(WORKLOADS)} workloads × {args.iterations} iterations against each server...", file=sys.stderr)

    for name, fn in WORKLOADS:
        print(f"  {name} ...", file=sys.stderr, end=" ", flush=True)
        with tempfile.TemporaryDirectory(prefix="secantus-bench-") as sec_dir, \
             tempfile.TemporaryDirectory(prefix="mongod-bench-") as mon_dir:
            with secantus_daemon(Path(sec_dir)) as sec_uri:
                sec_med, sec_p95 = time_workload(sec_uri, fn, args.iterations)
            with mongod_daemon(Path(mon_dir)) as mon_uri:
                mon_med, mon_p95 = time_workload(mon_uri, fn, args.iterations)
        ratio = sec_med / mon_med if mon_med > 0 else float("inf")
        print(f"sec={fmt_ms(sec_med)}ms  mon={fmt_ms(mon_med)}ms  (sec/mon = {ratio:.2f}×)", file=sys.stderr)
        results.append((name, sec_med, sec_p95, mon_med, mon_p95))

    # Markdown report.
    import datetime as dt
    import platform
    md: list[str] = []
    md.append("# SecantusDB vs mongod benchmark")
    md.append("")
    md.append(
        f"Generated {dt.date.today().isoformat()} on "
        f"{platform.system()} {platform.machine()} "
        f"({platform.processor() or 'unknown CPU'})."
    )
    md.append("")
    md.append(
        "Both servers use the **same WiredTiger storage engine** — mongod "
        "ships it; SecantusDB vendors the same C library — driven by the "
        "same `pymongo` client over the wire protocol. The hot path differs "
        "only above the storage layer (command dispatch, query planner, "
        "aggregation pipeline), so this is a fair comparison of the parts "
        "of SecantusDB that aren't WiredTiger itself."
    )
    md.append("")
    md.append(
        "Each workload runs against a freshly-spawned daemon on a free port "
        "with its own tmp data dir, both on-disk WiredTiger. "
        f"Each timed {args.iterations}× per server; we report median + p95 "
        f"in milliseconds. Dataset is {N_DOCS:,} small docs."
    )
    md.append("")
    md.append("## Results")
    md.append("")
    md.append("| Workload | SecantusDB median | SecantusDB p95 | mongod median | mongod p95 | sec/mongod |")
    md.append("|---|---:|---:|---:|---:|---:|")
    for name, sec_m, sec_p, mon_m, mon_p in results:
        ratio = sec_m / mon_m if mon_m > 0 else float("inf")
        md.append(
            f"| {name} | {fmt_ms(sec_m)} ms | {fmt_ms(sec_p)} ms | "
            f"{fmt_ms(mon_m)} ms | {fmt_ms(mon_p)} ms | "
            f"{ratio:.2f}× |"
        )
    md.append("")
    md.append("## How to read this")
    md.append("")
    md.append(
        "**`sec/mongod` < 1.0×** = SecantusDB is faster on this workload. "
        "**> 1.0×** = mongod is faster, by that ratio. "
        "1.0× = parity."
    )
    md.append("")
    md.append(
        "Both servers are running real WiredTiger on the same machine "
        "against the same dataset. Differences come from the layers above "
        "WT — SecantusDB's Python command dispatch and pure-Python query "
        "planner / aggregation pipeline vs mongod's compiled C++ versions."
    )
    md.append("")
    md.append(
        "The numbers above are **single-machine, single-process, no "
        "concurrency** — a deliberately narrow scenario to isolate "
        "per-operation latency. Throughput under concurrent connections "
        "is a separate measurement (and a place where mongod's connection "
        "pooling / async accept loop is going to win regardless)."
    )
    md.append("")
    md.append("## How to refresh")
    md.append("")
    md.append("```bash")
    md.append("uv run --no-sync python -m bench.compare_mongod")
    md.append("```")
    md.append("")
    md.append(
        "Requires `mongod` on `PATH` (Community Server is enough). "
        "On macOS: `brew tap mongodb/brew && brew install mongodb-community`. "
        "On Linux: see https://www.mongodb.com/docs/manual/installation/."
    )
    md.append("")

    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text("\n".join(md))
    print(f"\nWrote {args.out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
