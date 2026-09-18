#!/usr/bin/env python3
"""Statement throughput vs connection count: secantusd-pg against PostgreSQL 16.

The PG-side counterpart of `bench/concurrency.py`, and the instrument that
settled what the storage-concurrency backlog item is actually about. It answers
one question -- does throughput rise with N? -- for both servers on the same
box, because the reference for this server is the real product and never the
other SecantusDB server.

Each client is a separate PROCESS with its own connection: threads would share
a GIL and measure Python rather than the server. Every client runs a fixed
wall-clock loop and reports how many statements committed. `--mode distinct`
gives each client its own table (best case); `--mode shared` puts them all on
one table, which is where a global write lock would show itself.

**Build the RELEASE binary before quoting a number.** A debug `secantusd-pg` is
roughly 2.3x slower here, which is large enough to invert a conclusion about
absolute cost (scaling RATIOS survive it, absolute throughput does not):

    cd crates/secantus-pgserver && cargo build --release

Report `--repeat 3` or more. A single 5s window moved by up to 8% run to run on
this box, so a one-shot difference under that is not a difference.
"""

from __future__ import annotations

import argparse
import json
import multiprocessing as mp
import os
import signal
import socket
import statistics
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass
from pathlib import Path

REPO = Path("/Users/jdrumgoole/GIT/SecantusDB")
RUST_BINARY = REPO / "crates/secantus-pgserver/target/release/secantusd-pg"


@dataclass
class Result:
    clients: int
    committed: int
    seconds: float

    @property
    def ops_per_s(self) -> float:
        return self.committed / self.seconds if self.seconds else 0.0


@dataclass
class Trials:
    """Every repeat for one client count. Median, because a slow outlier from
    an unrelated process on the box should not move the reported figure."""

    clients: int
    rates: list[float]

    @property
    def median(self) -> float:
        return statistics.median(self.rates)

    @property
    def spread_pct(self) -> float:
        """Peak-to-peak as a percentage of the median -- the number that says
        whether a difference between two rows is real."""
        if len(self.rates) < 2 or not self.median:
            return 0.0
        return 100.0 * (max(self.rates) - min(self.rates)) / self.median


def _free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return int(s.getsockname()[1])


def _wait_for_listener(host: str, port: int, timeout: float = 30.0) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            with socket.create_connection((host, port), timeout=1.0):
                return
        except OSError:
            time.sleep(0.05)
    raise RuntimeError(f"no listener on {host}:{port} after {timeout}s")


def _worker(dsn: str, table: str, workload: str, seconds: float, out: mp.Queue) -> None:
    """Commit as many statements as possible for `seconds`, then report."""
    import psycopg  # imported in the child so the parent needn't hold a connection

    signal.signal(signal.SIGINT, signal.SIG_IGN)  # the parent owns Ctrl-C
    n = 0
    try:
        with psycopg.connect(dsn, autocommit=True) as conn, conn.cursor() as cur:
            key = os.getpid() * 10_000_000
            end = time.monotonic() + seconds
            while time.monotonic() < end:
                if workload == "insert":
                    cur.execute(f"insert into {table} (k, v) values (%s, %s)", (key + n, n))
                elif workload == "update":
                    cur.execute(f"update {table} set v = v + 1 where k = %s", (key,))
                else:
                    cur.execute(f"select v from {table} where k = %s", (key,))
                n += 1
    except Exception as exc:  # a failure must not read as zero throughput
        out.put(("error", f"{type(exc).__name__}: {exc}"))
        return
    out.put(("ok", n))


def _prepare(dsn: str, tables: list[str], workload: str) -> None:
    import psycopg

    with psycopg.connect(dsn, autocommit=True) as conn, conn.cursor() as cur:
        for t in tables:
            cur.execute(f"drop table if exists {t}")
            cur.execute(f"create table {t} (k bigint primary key, v bigint)")
        if workload in ("update", "select"):
            # Seed one row per client so the statement has a target.
            for t in tables:
                for pid_slot in range(64):
                    cur.execute(f"insert into {t} (k, v) values (%s, 0)", (pid_slot,))


def _run_one(dsn: str, clients: int, mode: str, workload: str, seconds: float) -> Result:
    tables = [f"bench_{i}" for i in range(clients)] if mode == "distinct" else ["bench_shared"]
    _prepare(dsn, tables, workload)
    q: mp.Queue = mp.Queue()
    procs = [
        mp.Process(target=_worker, args=(dsn, tables[i % len(tables)], workload, seconds, q))
        for i in range(clients)
    ]
    start = time.monotonic()
    for p in procs:
        p.start()
    total, errors = 0, []
    for _ in procs:
        kind, payload = q.get()
        if kind == "error":
            errors.append(payload)
        else:
            total += int(payload)
    for p in procs:
        p.join()
    elapsed = time.monotonic() - start
    if errors:
        raise RuntimeError(f"{len(errors)} client(s) failed, first: {errors[0]}")
    return Result(clients, total, elapsed)


def _sweep(
    label: str,
    dsn: str,
    counts: list[int],
    mode: str,
    workload: str,
    secs: float,
    repeat: int,
) -> list[Trials]:
    plural = "s" if mode == "distinct" else ""
    print(f"\n== {label}  ({workload}, {mode} table{plural}, {secs}s x{repeat})")
    print(f"{'clients':>8} {'ops/s':>10} {'spread':>8} {'scaling':>9}")
    out: list[Trials] = []
    base = None
    for n in counts:
        rates = [_run_one(dsn, n, mode, workload, secs).ops_per_s for _ in range(repeat)]
        t = Trials(n, rates)
        base = base or t.median
        ratio = t.median / base if base else 0.0
        print(f"{n:>8} {t.median:>10,.0f} {t.spread_pct:>7.1f}% {ratio:>8.2f}x")
        out.append(t)
    return out


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--server", choices=["rust", "postgres", "both"], default="both")
    ap.add_argument("--clients", default="1,2,4,8", help="comma-separated client counts")
    ap.add_argument("--mode", choices=["distinct", "shared"], default="distinct")
    ap.add_argument("--workload", choices=["insert", "update", "select"], default="insert")
    ap.add_argument("--seconds", type=float, default=10.0)
    ap.add_argument("--repeat", type=int, default=3, help="trials per client count; median wins")
    ap.add_argument("--json", type=Path, default=None, help="also write the raw trials here")
    ap.add_argument("--pg-dsn", default="host=127.0.0.1 port=5432 dbname=postgres")
    args = ap.parse_args(argv)
    counts = [int(c) for c in args.clients.split(",")]

    daemon = None
    collected: dict[str, list[Trials]] = {}
    try:
        if args.server in ("postgres", "both"):
            collected["postgres"] = _sweep(
                "PostgreSQL 16",
                args.pg_dsn,
                counts,
                args.mode,
                args.workload,
                args.seconds,
                args.repeat,
            )
        if args.server in ("rust", "both"):
            port = _free_port()
            store = tempfile.mkdtemp(prefix="pgbench-")
            daemon = subprocess.Popen(
                [str(RUST_BINARY), store, f"127.0.0.1:{port}"],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )
            _wait_for_listener("127.0.0.1", port)
            dsn = f"host=127.0.0.1 port={port} dbname=postgres user=postgres"
            collected["rust"] = _sweep(
                "secantusd-pg",
                dsn,
                counts,
                args.mode,
                args.workload,
                args.seconds,
                args.repeat,
            )
    except KeyboardInterrupt:
        print("\ninterrupted", file=sys.stderr)
        return 130
    finally:
        if daemon is not None:
            daemon.terminate()
            try:
                daemon.wait(timeout=10)
            except subprocess.TimeoutExpired:
                daemon.kill()
    if args.json:
        args.json.write_text(
            json.dumps(
                {
                    "workload": args.workload,
                    "mode": args.mode,
                    "seconds": args.seconds,
                    "repeat": args.repeat,
                    "servers": {
                        k: [{"clients": t.clients, "rates": t.rates} for t in v]
                        for k, v in collected.items()
                    },
                },
                indent=2,
            )
            + "\n"
        )
    if "postgres" in collected and "rust" in collected:
        print("\n== secantusd-pg relative to PostgreSQL 16 (median ops/s)")
        pg = {t.clients: t.median for t in collected["postgres"]}
        for t in collected["rust"]:
            if t.clients in pg and t.median:
                print(f"{t.clients:>8} clients: {pg[t.clients] / t.median:>5.2f}x slower")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
