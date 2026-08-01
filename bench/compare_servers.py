"""Compare throughput of the Rust server, the Python server, and real mongod.

SecantusDB ships two servers (see CLAUDE.md "Engines: ... TWO SEPARATE
SERVERS"): the pure-Python ``SecantusDBServer`` and the pure-Rust server
(``_secantus_server.RustServer``). This harness runs the same workloads the
perf-regression suite uses — insert, indexed-range find, full scan,
update-many, ``$group`` aggregate, delete-many — against both, plus a real
single-node ``mongod`` when one is on ``PATH`` (all three on real on-disk
WiredTiger, driven through ``pymongo`` so each number includes the wire +
driver overhead a real client pays). ``mongod`` is the reference: the table
reports each server's median time and how many times slower than ``mongod`` it
is, so you can see both the Rust-vs-Python gap and how close each gets to the
real thing.

The Rust server is **not** in the default wheel — it must be compiled first:

    SKBUILD_CMAKE_DEFINE=SECANTUS_BUILD_STORAGE_ENGINE=ON uv sync --extra dev

``mongod`` is optional: if it isn't on ``PATH`` (and no ``--mongo-uri`` is
given), the comparison runs Rust-vs-Python only and notes mongod was skipped.

Run via ``uv run python -m invoke compare-servers`` (or this module directly):

    uv run --no-sync python -m bench.compare_servers --n 10000 --reps 5
    uv run --no-sync python -m bench.compare_servers --n 100000   # bigger, see how the gap scales
    uv run --no-sync python -m bench.compare_servers --mongo-uri mongodb://127.0.0.1:27017
    uv run --no-sync python -m bench.compare_servers --no-mongod

Ctrl-C aborts cleanly between reps.
"""

from __future__ import annotations

import argparse
import shutil
import signal
import socket
import statistics
import subprocess
import sys
import tempfile
import time
from collections.abc import Iterator
from contextlib import contextmanager
from pathlib import Path
from typing import Any

import pymongo

# Workload order = display order. Each runs once per rep against a fresh
# collection; the per-workload median across reps is reported.
WORKLOADS = (
    "insert",
    "find_indexed_range",
    "find_all",
    "find_filtered_scan",
    "update_many_half",
    "aggregate_group",
    "aggregate_multistage",
    "delete_many_half",
    "change_stream_drain",
)


def _make_docs(n: int) -> list[dict[str, Any]]:
    # Same shape as tests/test_perf_regression.py::make_docs.
    return [{"_id": i, "i": i, "g": i % 50, "v": i * 2, "active": i % 2 == 0} for i in range(n)]


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
def _rust_client() -> Iterator[pymongo.MongoClient]:
    import _secantus_server

    srv = _secantus_server.RustServer(
        tempfile.mkdtemp(prefix="cmp-rust-"), 0, replica_set_name="secantus"
    )
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


@contextmanager
def _mongod_replset_client() -> Iterator[pymongo.MongoClient]:
    """Spawn a throwaway SINGLE-NODE REPLICA SET mongod (change streams need
    one) — used only for the ``change_stream_drain`` reference number; every
    other row keeps the standalone reference so the table stays comparable
    with earlier publications."""
    data_dir = Path(tempfile.mkdtemp(prefix="cmp-mongod-rs-"))
    port = _free_port()
    proc = subprocess.Popen(
        [
            "mongod",
            "--bind_ip",
            "127.0.0.1",
            "--port",
            str(port),
            "--dbpath",
            str(data_dir),
            "--logpath",
            str(data_dir / "mongod.log"),
            "--replSet",
            "cmp0",
            "--noauth",
            "--quiet",
        ],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    try:
        _wait_for_listener("127.0.0.1", port)
        client = pymongo.MongoClient(
            f"mongodb://127.0.0.1:{port}", directConnection=True, serverSelectionTimeoutMS=10000
        )
        client.admin.command("replSetInitiate")
        deadline = time.perf_counter() + 30
        while time.perf_counter() < deadline:
            try:
                if client.admin.command("hello").get("isWritablePrimary"):
                    break
            except pymongo.errors.PyMongoError:
                pass
            time.sleep(0.2)
        yield client
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()
        shutil.rmtree(data_dir, ignore_errors=True)


@contextmanager
def _mongod_client(mongo_uri: str | None) -> Iterator[pymongo.MongoClient]:
    """Connect to ``--mongo-uri`` if given, else spawn a throwaway single-node
    ``mongod`` on a free port + temp dbpath (cleaned up on exit)."""
    if mongo_uri:
        yield pymongo.MongoClient(mongo_uri, serverSelectionTimeoutMS=5000)
        return
    data_dir = Path(tempfile.mkdtemp(prefix="cmp-mongod-"))
    port = _free_port()
    proc = subprocess.Popen(
        [
            "mongod",
            "--bind_ip",
            "127.0.0.1",
            "--port",
            str(port),
            "--dbpath",
            str(data_dir),
            "--logpath",
            str(data_dir / "mongod.log"),
            "--noauth",
            "--quiet",
        ],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    try:
        _wait_for_listener("127.0.0.1", port)
        time.sleep(0.5)  # mongod accepts TCP before the handshake is fully ready
        yield pymongo.MongoClient(f"mongodb://127.0.0.1:{port}", serverSelectionTimeoutMS=5000)
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()
        shutil.rmtree(data_dir, ignore_errors=True)


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

    # COLLSCAN with a numeric range filter on an UNINDEXED field (`g` — only
    # `v` is indexed): the per-document compare path, which `find_all` (no
    # filter) and the indexed range (B-tree byte compare) never touch.
    start = time.perf_counter()
    _ = list(coll.find({"g": {"$gte": 25}}))
    t["find_filtered_scan"] = time.perf_counter() - start

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

    # Multi-stage pipeline over a separate array-bearing collection. `$match`
    # (lifted into the fetch) -> `$unwind` (fans each doc out ~3x) -> `$group`
    # -> `$sort`: the ~3n/2 documents flowing out of `$unwind` into `$group`
    # are the large *inter-stage* intermediate a streaming execution model
    # (Phase 6b) would avoid materializing. The single-stage `aggregate_group`
    # above can't show that cost, and 6a's field pushdown doesn't apply here
    # (the first heavier stage is `$unwind`, not `$group`). The collection is
    # populated untimed so only the pipeline is measured.
    cm = client["perf"]["cm"]
    cm.drop()
    cm.insert_many(
        [
            {
                "_id": i,
                "g": i % 50,
                "v": i * 2,
                "active": i % 2 == 0,
                "tags": [i % 10, (i + 1) % 10, (i + 2) % 10],
            }
            for i in range(n)
        ],
        ordered=True,
    )
    start = time.perf_counter()
    _ = list(
        cm.aggregate(
            [
                {"$match": {"active": True}},
                {"$unwind": "$tags"},
                {"$group": {"_id": "$tags", "total": {"$sum": "$v"}, "n": {"$sum": 1}}},
                {"$sort": {"total": -1}},
            ]
        )
    )
    t["aggregate_multistage"] = time.perf_counter() - start
    cm.drop()

    start = time.perf_counter()
    coll.delete_many({"active": False})
    t["delete_many_half"] = time.perf_counter() - start

    coll.drop()
    t["change_stream_drain"] = _change_stream_drain(client, n)
    return t


def _change_stream_drain(client: pymongo.MongoClient, n: int) -> float:
    """Seconds to drain ``n // 2`` change-stream events (inserted while the
    watch is open; only the drain is timed). ``NaN`` when the server rejects
    ``$changeStream`` — a STANDALONE mongod does (its change streams need a
    replica set), so the throwaway standalone reference gets its number from
    a separate single-node-replica-set spawn in ``main``."""
    events = n // 2
    cs = client["perf"]["cs"]
    cs.drop()
    try:
        with cs.watch(batch_size=2000) as stream:
            for lo in range(0, events, 1000):
                cs.insert_many([{"_id": lo + k, "pad": "y" * 64} for k in range(1000)])
            start = time.perf_counter()
            for _ in range(events):
                stream.next()
            elapsed = time.perf_counter() - start
    except pymongo.errors.PyMongoError:
        return float("nan")
    finally:
        cs.drop()
    return elapsed


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
    parser.add_argument(
        "--mongo-uri", default="", help="existing mongod URI (default: spawn a throwaway mongod)"
    )
    parser.add_argument("--no-mongod", action="store_true", help="skip the mongod comparison")
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

    use_mongod = not args.no_mongod and (bool(args.mongo_uri) or shutil.which("mongod") is not None)
    if not args.no_mongod and not use_mongod:
        print("note: mongod not on PATH and no --mongo-uri — skipping the mongod column.\n")

    print(f"workload n={args.n}, median of {args.reps} reps, on-disk WiredTiger, via pymongo\n")
    mongod = (
        _median_run(lambda: _mongod_client(args.mongo_uri or None), args.n, args.reps)
        if use_mongod
        else None
    )
    import math

    if (
        mongod is not None
        and math.isnan(mongod.get("change_stream_drain", float("nan")))
        and not args.mongo_uri
        and shutil.which("mongod") is not None
    ):
        # Standalone mongod rejects $changeStream — measure that one row
        # against a throwaway single-node replica set.
        samples = []
        for _ in range(args.reps):
            with _mongod_replset_client() as rc:
                samples.append(_change_stream_drain(rc, args.n))
        mongod["change_stream_drain"] = statistics.median(samples)
    rust = _median_run(_rust_client, args.n, args.reps)
    py = _median_run(_python_client, args.n, args.reps)

    # Column labels: SecantusDB = the Python server, SecantusDB-rs = the Rust
    # server. The implementation is spelled out in a sub-label row beneath.
    if mongod is not None:
        # mongod is the reference: show each server's ms + how many x slower.
        header = (
            f"{'workload':<22}{'mongod(ms)':>12}{'SecantusDB-rs(ms)':>19}"
            f"{'×mongod':>9}{'SecantusDB(ms)':>16}{'×mongod':>9}"
        )
        sub = f"{'':<22}{'':>12}{'(Rust)':>19}{'':>9}{'(Python)':>16}{'':>9}"
        print(header)
        print(sub)
        print("-" * len(header))
        for k in WORKLOADS:
            m, r, p = mongod[k] * 1000, rust[k] * 1000, py[k] * 1000
            rx = r / m if m else float("nan")
            px = p / m if m else float("nan")
            row = f"{k:<22}{m:>12.2f}{r:>19.2f}{rx:>8.1f}x{p:>16.2f}{px:>8.1f}x"
            print(row.replace("nan", "  —"))
    else:
        header = f"{'workload':<22}{'SecantusDB-rs(ms)':>19}{'SecantusDB(ms)':>16}{'speedup':>10}"
        sub = f"{'':<22}{'(Rust)':>19}{'(Python)':>16}{'':>10}"
        print(header)
        print(sub)
        print("-" * len(header))
        for k in WORKLOADS:
            r, p = rust[k] * 1000, py[k] * 1000
            print(f"{k:<22}{r:>19.2f}{p:>16.2f}{(p / r if r else float('nan')):>9.1f}x")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
