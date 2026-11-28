"""Gap test of RP-1227: a join holds every joined row in memory before the
aggregate (or the client) sees the first.

PostgreSQL 15.20 (a private cluster) runs `select count(*) from e join f
on e.a > f.a` over `e` (2,000 rows) and `f` (100) as an Aggregate over a
Nested Loop with a Join Filter, `f` under a Materialize: it holds the 100
inner rows and none of the 194,950 joined ones, and answers in 15 ms.

`secantusd-pg` (debug, tree 02293a75, 2026-11-28) answers the same count,
but its RSS rose 227 MB while it did, about 1.1 KB per joined row; the
same join over 20,000 x 200 rows (3,979,900 rows) rose 3.9 GB in 41 s, and
RP-919's three-table inequality chain over 1,000-row tables (4,994,835
rows) 3.6 GB in 62 s, where PostgreSQL took 0.4 s. It is not the
inequality alone: `count(*), sum(e.a + f.a)` over `e.b = f.b` (200,000
rows) rose 128 MB, and every shape measured rose the same with
`SECANTUS_PG_JOIN_STREAM=0`, so the bounded-memory join of batch 65
(`stream_join.rs`) bounded none of them. Its tests compare rows, not
memory.

The server's peak RSS is sampled with `ps` (Linux and macOS) while the
count runs. The bound, 64 MB, is under a third of what the join holds
today and far above a streamed join's (its right side plus a batch).
"""

from __future__ import annotations

import subprocess
import threading
from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

SETUP = [
    "create table e (a int, b text)",
    "insert into e select g, 'v' || (g % 10) from generate_series(1, 2000) g",
    "create table f (a int, b text)",
    "insert into f select g, 'v' || (g % 10) from generate_series(1, 100) g",
    "analyze e",
    "analyze f",
]

BOUND_MB = 64


def _rss_mb(pid: int) -> int:
    out = subprocess.run(["ps", "-o", "rss=", "-p", str(pid)], capture_output=True, text=True)
    return int(out.stdout.strip() or 0) // 1024


@pytest.mark.backlog("RP-1227")
def test_an_inequality_join_does_not_hold_its_rows(rust_pg: Any) -> None:
    pid = rust_pg.proc.pid
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        c.execute("drop table if exists e, f")
        for s in SETUP:
            c.execute(s)
        before = _rss_mb(pid)
        peak = [before]
        done = threading.Event()

        def sample() -> None:
            while not done.is_set():
                peak[0] = max(peak[0], _rss_mb(pid))
                done.wait(0.05)

        t = threading.Thread(target=sample, daemon=True)
        t.start()
        try:
            (n,) = c.execute("select count(*) from e join f on e.a > f.a").fetchone()
        finally:
            done.set()
            t.join()
        peak[0] = max(peak[0], _rss_mb(pid))
    assert n == 194950
    assert peak[0] - before < BOUND_MB, (
        f"the server's RSS rose {peak[0] - before} MB ({before} -> {peak[0]} MB) "
        f"counting 194,950 joined rows; PostgreSQL holds only the 100 inner rows"
    )
