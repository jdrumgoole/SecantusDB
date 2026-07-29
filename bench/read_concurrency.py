"""Read-concurrency measurement: how much reader throughput survives write load.

`bench.concurrency` measures *write* scaling and is write-only, so it can't
show what taking reads off the storage write lock bought. This does: it
measures aggregate reader throughput (find() batches/s across R reader
threads) in two conditions on the SAME running server —

  (1) readers alone
  (2) readers contending with W writer threads hammering inserts

and reports condition (2) as a percentage of (1). When reads don't serialize
behind writes, that retention stays high even as writers saturate. pymongo
releases the GIL on socket I/O, so N threads drive N concurrent server
requests — the bottleneck is server-side, not the client's GIL.

    uv run python -m bench.read_concurrency --server rust --readers 4 --writers 8

Measured 2026-07-17 on the Rust server (per-collection write locks): ~60-75%
of standalone reader throughput retained under 4-8 saturating writers. See
tasks/rust-perf-findings.md.
"""

from __future__ import annotations

import argparse
import tempfile
import threading
import time
from pathlib import Path

from pymongo import MongoClient

from bench.concurrency import _free_port, _spawn_server, _wait_listen

PAYLOAD = "x" * 512


def seed(uri: str, n: int) -> None:
    c = MongoClient(uri)
    coll = c["probe"]["docs"]
    coll.drop()
    coll.insert_many([{"_id": i, "g": i % 20, "p": PAYLOAD} for i in range(n)])
    coll.create_index("g")
    c.close()


def reader_loop(uri: str, stop: threading.Event, out: list[int], idx: int) -> None:
    c = MongoClient(uri)
    coll = c["probe"]["docs"]
    n = 0
    g = idx % 20
    while not stop.is_set():
        list(coll.find({"g": g}))
        n += 1
    out[idx] = n
    c.close()


def writer_loop(uri: str, stop: threading.Event, idx: int) -> None:
    c = MongoClient(uri)
    coll = c["probe"][f"w{idx}"]
    k = 0
    while not stop.is_set():
        coll.insert_many([{"n": k + j, "p": PAYLOAD} for j in range(50)])
        k += 50
    c.close()


def measure(uri: str, readers: int, writers: int, duration: float) -> float:
    stop = threading.Event()
    rout = [0] * readers
    rthreads = [
        threading.Thread(target=reader_loop, args=(uri, stop, rout, i)) for i in range(readers)
    ]
    wthreads = [threading.Thread(target=writer_loop, args=(uri, stop, i)) for i in range(writers)]
    for t in rthreads + wthreads:
        t.start()
    time.sleep(duration)
    stop.set()
    for t in rthreads + wthreads:
        t.join()
    return sum(rout) / duration


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--server", default="rust")
    ap.add_argument("--readers", type=int, default=4)
    ap.add_argument("--writers", type=int, default=4)
    ap.add_argument("--duration", type=float, default=15.0)
    ap.add_argument("--seed-docs", type=int, default=5000)
    args = ap.parse_args()

    storage = Path(tempfile.mkdtemp(prefix="read-conc-probe-"))
    port = _free_port()
    proc = _spawn_server(port, storage, args.server, None)
    try:
        assert _wait_listen("127.0.0.1", port, timeout=30), "server didn't come up"
        uri = f"mongodb://127.0.0.1:{port}/"
        seed(uri, args.seed_docs)

        alone = measure(uri, args.readers, 0, args.duration)
        contended = measure(uri, args.readers, args.writers, args.duration)

        print(f"server={args.server}  readers={args.readers}  writers={args.writers}")
        print(f"  readers alone:              {alone:8.1f} find-batches/s")
        print(f"  readers + {args.writers} writers:      {contended:8.1f} find-batches/s")
        ratio = contended / alone if alone else 0.0
        print(f"  retained under write load:  {ratio * 100:5.1f}%")
    finally:
        proc.terminate()
        proc.wait()


if __name__ == "__main__":
    main()
