#!/usr/bin/env python3
"""Bisect the per-statement cost of secantusd-pg by removing one layer at a time.

The companion to `bench/pg_concurrency.py`: that one says how throughput scales,
this one says where a single statement's time goes. It exists because the
scaling curve pointed at per-statement cost and nothing in the tree could say
which layer owned it.

Each stage differs from the one above by a single ingredient, so the DIFFERENCE
between two rows is that ingredient's cost. Run single-client so nothing is
confounded by concurrency, and report microseconds of wall time per statement
(the client is otherwise idle, so wall ~ server cost + loopback).

  ping           protocol floor: no SQL at all
  select_const   + parse + plan of `SELECT 1`
  select_const_p same, but the statement is PREPARED once and re-executed
  select_row     + catalog lookup + storage read of one row by primary key
  select_row_p   same, prepared

`select 1` is the load-bearing row: it touches no table, so everything it costs
above the protocol floor is overhead. Measured 2026-09-18 it cost 54.5us against
PostgreSQL's 9.1us, which is the whole gap.

Add `--in-transaction` to run the same statements inside one explicit block.
Note what that flag really varies: the harness creates its table inside the
block too, so it measures a block THAT HAS DONE DDL, not a bare block. That
distinction was the whole of a 2026-09-19 investigation -- a block on its own
cost slightly LESS than autocommit, while a block holding uncommitted DDL cost
three times as much, because the uncommitted-type overlay disabled both the
process-wide catalog cache and the planner's type-table skip. Run it both ways
(`conn.commit()` after the DDL) before attributing anything to "being in a
transaction".

**Release binary only.** A debug build is ~2.3x slower and will mislead you.
Set `SECANTUSD_PG` to measure a binary other than the main checkout's: a
worktree builds its own, and the hardcoded path below would silently measure
`main` instead of the branch under test.
"""

from __future__ import annotations

import argparse
import os
import socket
import statistics
import subprocess
import tempfile
import time
from pathlib import Path

REPO = Path("/Users/jdrumgoole/GIT/SecantusDB")
RUST = Path(
    os.environ.get("SECANTUSD_PG", REPO / "crates/secantus-pgserver/target/release/secantusd-pg")
)


def _free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return int(s.getsockname()[1])


def _wait(host: str, port: int, timeout: float = 30.0) -> None:
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        try:
            with socket.create_connection((host, port), timeout=1.0):
                return
        except OSError:
            time.sleep(0.05)
    raise RuntimeError("no listener")


def _time_loop(fn, iters: int) -> float:
    """Median microseconds per call over 5 batches (median kills outliers)."""
    batches = []
    for _ in range(5):
        t0 = time.perf_counter()
        for _ in range(iters):
            fn()
        batches.append((time.perf_counter() - t0) / iters * 1e6)
    return statistics.median(batches)


def measure(dsn: str, iters: int, in_transaction: bool = False) -> dict[str, float]:
    import psycopg

    out: dict[str, float] = {}
    with psycopg.connect(dsn, autocommit=not in_transaction) as conn:
        with conn.cursor() as cur:
            cur.execute("drop table if exists attr_t")
            cur.execute("create table attr_t (k bigint primary key, v bigint)")
            cur.execute("insert into attr_t (k, v) values (1, 42)")

        # Protocol floor: psycopg's own no-op round trip.
        out["ping"] = _time_loop(lambda: conn.pgconn.exec_(b" ").status, iters)

        with conn.cursor() as cur:
            out["select_const"] = _time_loop(lambda: cur.execute("select 1"), iters)
            out["select_row"] = _time_loop(
                lambda: cur.execute("select v from attr_t where k = 1"), iters
            )
            # prepare_threshold=0 makes psycopg prepare on first use and reuse.
            out["select_const_p"] = _time_loop(lambda: cur.execute("select 1", prepare=True), iters)
            out["select_row_p"] = _time_loop(
                lambda: cur.execute("select v from attr_t where k = %s", (1,), prepare=True), iters
            )
    return out


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--iters", type=int, default=2000)
    ap.add_argument(
        "--in-transaction",
        action="store_true",
        help="run inside one explicit block instead of autocommit",
    )
    ap.add_argument("--pg-dsn", default="host=127.0.0.1 port=5432 dbname=postgres")
    args = ap.parse_args()

    port = _free_port()
    store = tempfile.mkdtemp(prefix="attr-")
    d = subprocess.Popen(
        [str(RUST), store, f"127.0.0.1:{port}"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    try:
        _wait("127.0.0.1", port)
        dsn = f"host=127.0.0.1 port={port} dbname=postgres user=postgres"
        ours = measure(dsn, args.iters, args.in_transaction)
        pg = measure(args.pg_dsn, args.iters, args.in_transaction)
    except KeyboardInterrupt:
        return 130
    finally:
        d.terminate()
        try:
            d.wait(timeout=10)
        except subprocess.TimeoutExpired:
            d.kill()

    stages = ["ping", "select_const", "select_const_p", "select_row", "select_row_p"]
    print(f"{'stage':>16} {'ours us':>9} {'PG us':>8} {'delta':>8}")
    for s in stages:
        print(f"{s:>16} {ours[s]:>9.1f} {pg[s]:>8.1f} {ours[s] - pg[s]:>+8.1f}")

    print("\n-- what each step ADDS (ours / PG)")
    steps = [
        ("parse+plan `select 1`", "select_const", "ping"),
        ("...saved by preparing", "select_const", "select_const_p"),
        ("catalog+storage row read", "select_row", "select_const"),
        ("...saved by preparing", "select_row", "select_row_p"),
    ]
    for label, a, b in steps:
        print(f"{label:>28}: {ours[a] - ours[b]:>+7.1f}us / {pg[a] - pg[b]:>+6.1f}us")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
