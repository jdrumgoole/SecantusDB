"""Compare throughput of the Rust server vs the Python server.

SecantusDB ships two servers (see CLAUDE.md "Engines: ... TWO SEPARATE
SERVERS"): the pure-Python ``SecantusDBServer`` and the pure-Rust server
(``_secantus_server.RustServer``). This harness runs the same workloads the
perf-regression suite uses — insert, indexed-range find, full scan,
update-many, ``$group`` aggregate, delete-many — against both, over real
on-disk WiredTiger, driven through ``pymongo`` so each number includes the
wire + driver overhead a real client pays. It reports the median time per
workload and the Rust-vs-Python speedup, so a regression in either server's
relative performance is visible at a glance.

The Rust server is **not** in the default wheel — it must be compiled first:

    SKBUILD_CMAKE_DEFINE=SECANTUS_BUILD_STORAGE_ENGINE=ON uv sync --extra dev

Run via ``uv run python -m invoke compare-servers`` (or this module directly):

    uv run --no-sync python -m bench.compare_servers --n 10000 --reps 5
    uv run --no-sync python -m bench.compare_servers --n 100000   # bigger, see how the gap scales

Ctrl-C aborts cleanly between reps.
"""

from __future__ import annotations

import argparse
import signal
import statistics
import sys
import tempfile
import time
from collections.abc import Iterator
from contextlib import contextmanager
from typing import Any

import pymongo

# Workload order = display order. Each runs once per rep against a fresh
# collection; the per-workload median across reps is reported.
WORKLOADS = (
    "insert",
    "find_indexed_range",
    "find_all",
    "update_many_half",
    "aggregate_group",
    "delete_many_half",
)


def _make_docs(n: int) -> list[dict[str, Any]]:
    # Same shape as tests/test_perf_regression.py::make_docs.
    return [{"_id": i, "i": i, "g": i % 50, "v": i * 2, "active": i % 2 == 0} for i in range(n)]


@contextmanager
def _rust_client() -> Iterator[pymongo.MongoClient]:
    import _secantus_server

    srv = _secantus_server.RustServer(tempfile.mkdtemp(prefix="cmp-rust-"), 0)
    try:
        host, port = srv.address
        yield pymongo.MongoClient(host, port, directConnection=True, serverSelectionTimeoutMS=5000)
    finally:
        srv.stop()


@contextmanager
def _python_client() -> Iterator[pymongo.MongoClient]:
    from secantus import SecantusDBServer

    srv = SecantusDBServer(port=0, storage_path=tempfile.mkdtemp(prefix="cmp-py-"))
    srv.start()
    try:
        yield pymongo.MongoClient(srv.uri, directConnection=True, serverSelectionTimeoutMS=5000)
    finally:
        srv.stop()


def _run_workloads(client: pymongo.MongoClient, n: int) -> dict[str, float]:
    """Time each workload once against a fresh collection; seconds per workload."""
    coll = client["perf"]["c"]
    coll.drop()
    docs = _make_docs(n)
    t: dict[str, float] = {}

    start = time.perf_counter()
    coll.insert_many(docs, ordered=True)
    t["insert"] = time.perf_counter() - start

    coll.create_index("v")
    start = time.perf_counter()
    _ = list(coll.find({"v": {"$gte": n, "$lt": n + n // 2}}))
    t["find_indexed_range"] = time.perf_counter() - start

    start = time.perf_counter()
    _ = list(coll.find({}))
    t["find_all"] = time.perf_counter() - start

    start = time.perf_counter()
    coll.update_many({"active": True}, {"$inc": {"v": 1}})
    t["update_many_half"] = time.perf_counter() - start

    start = time.perf_counter()
    _ = list(
        coll.aggregate(
            [
                {
                    "$group": {
                        "_id": "$g",
                        "sum": {"$sum": "$v"},
                        "avg": {"$avg": "$v"},
                        "min": {"$min": "$v"},
                        "max": {"$max": "$v"},
                    }
                }
            ]
        )
    )
    t["aggregate_group"] = time.perf_counter() - start

    start = time.perf_counter()
    coll.delete_many({"active": False})
    t["delete_many_half"] = time.perf_counter() - start

    coll.drop()
    return t


def _median_run(make_client: Any, n: int, reps: int) -> dict[str, float]:
    samples: dict[str, list[float]] = {}
    for _ in range(reps):
        with make_client() as client:
            for k, v in _run_workloads(client, n).items():
                samples.setdefault(k, []).append(v)
    return {k: statistics.median(v) for k, v in samples.items()}


def main(argv: list[str] | None = None) -> int:
    signal.signal(signal.SIGINT, lambda *_: sys.exit(130))
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument(
        "--n", type=int, default=10_000, help="documents per workload (default: 10000)"
    )
    parser.add_argument(
        "--reps", type=int, default=5, help="reps to take the median over (default: 5)"
    )
    args = parser.parse_args(argv)

    try:
        import _secantus_server  # noqa: F401
    except ImportError:
        print(
            "The Rust server (_secantus_server) is not built. Build it with:\n"
            "    SKBUILD_CMAKE_DEFINE=SECANTUS_BUILD_STORAGE_ENGINE=ON uv sync --extra dev",
            file=sys.stderr,
        )
        return 2

    print(f"workload n={args.n}, median of {args.reps} reps, on-disk WiredTiger, via pymongo\n")
    rust = _median_run(_rust_client, args.n, args.reps)
    py = _median_run(_python_client, args.n, args.reps)

    print(f"{'workload':<22}{'rust (ms)':>12}{'python (ms)':>14}{'speedup':>12}")
    print("-" * 60)
    for k in WORKLOADS:
        r, p = rust[k] * 1000, py[k] * 1000
        speedup = p / r if r else float("nan")
        print(f"{k:<22}{r:>12.2f}{p:>14.2f}{speedup:>11.1f}x")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
