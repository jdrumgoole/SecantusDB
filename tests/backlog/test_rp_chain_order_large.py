"""The order of a three-way chain's rows over larger tables (found while
working RP-1114).

Both shapes are measured on PostgreSQL 15.20 (a private cluster,
2026-11-27), over tables never analysed:

- `t5 z LEFT JOIN (u1 x JOIN t4 y ON x.b = y.b) ON z.b = x.a` with `t5`
  300 rows, `u1` 1,000 and `t4` 3,000 is a Hash Left Join of `z` over a
  Hash of the Merge Join of `x` and `y`, whose 428,800 rows outgrow
  `work_mem` while the table is built: `Buckets: 262144 (originally
  16384)  Batches: 4 (originally 1)` (`ExecHashIncreaseNumBatches` moves
  rows out to batch files as it goes), so a key's rows come out in another
  order than one table holding them all.
- `u1 x JOIN t4 y ON x.b = y.b LEFT JOIN t5 z ON z.b = x.a` with `u1` 3,000
  rows, `t4` 300 and `t5` 3,000 is a Merge Join of `y` with a Hash Right
  Join of `z` and `x`; this server hashes `x` and `y` first.

Each run is in a database of its own, in a transaction it rolls back, so
no table is ever analysed.
"""

from __future__ import annotations

from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn


def _setup(n4: int, n5: int, n1: int) -> list[str]:
    return [
        "create table t4 (a int, b int, c text)",
        "insert into t4 select case when g % 9 = 0 then null else g % 500 end, g % 7,"
        f" 'x' || g from generate_series(1, {n4}) g",
        "create table t5 (a int, b int, c text)",
        "insert into t5 select case when g % 9 = 0 then null else g % 3 end, g % 7,"
        f" 'z' || repeat('w', g % 40) || g from generate_series(1, {n5}) g",
        "create table u1 (a int primary key, b int, c text)",
        f"insert into u1 select g, g % 5, 'u' || g from generate_series(1, {n1}) g",
    ]


def _rows(server: Any, setup: list[str], query: str) -> list[tuple]:
    with psycopg.connect(_dsn(server) + " dbname=postgres", autocommit=True) as c:
        c.execute("drop database if exists rp_chain_large")
        c.execute("create database rp_chain_large")
    with psycopg.connect(_dsn(server) + " dbname=rp_chain_large") as c:
        try:
            for s in setup:
                c.execute(s)
            return c.execute(query).fetchall()
        finally:
            c.rollback()


@pytest.mark.backlog("RP-1169")
def test_a_hash_table_that_grows_batches_returns_a_keys_rows_in_postgres_order(
    rust_pg: Any,
) -> None:
    """The rows of `x.c = 'u1'` in the order PostgreSQL 15.20 returns them."""
    rows = _rows(
        rust_pg,
        _setup(3000, 300, 1000),
        "select x.c, y.c, z.c from t5 z left join (u1 x join t4 y on x.b = y.b) on z.b = x.a",
    )
    assert len(rows) == 110681
    # Some keys' rows (`u6`'s) come out in PostgreSQL's order here; `u1`'s
    # do not.
    u1 = [r[1] for r in rows if r[0] == "u1"]
    assert len(u1) == 18447
    assert u1[:10] == [
        "x2136",
        "x862",
        "x2143",
        "x2878",
        "x855",
        "x2150",
        "x848",
        "x2157",
        "x2983",
        "x841",
    ]


@pytest.mark.backlog("RP-1170")
def test_a_left_chain_over_larger_tables_returns_rows_in_postgres_order(
    rust_pg: Any,
) -> None:
    """The first rows in the order PostgreSQL 15.20 returns them."""
    rows = _rows(
        rust_pg,
        _setup(300, 3000, 3000),
        "select x.c, y.c, z.c from u1 x join t4 y on x.b = y.b left join t5 z on z.b = x.a",
    )
    assert len(rows) == 238311
    assert rows[:6] == [
        ("u5", "x14", "z" + "w" * 26 + "586"),
        ("u5", "x14", "z" + "w" * 19 + "1699"),
        ("u5", "x14", "z" + "w" * 18 + "978"),
        ("u1190", "x14", None),
        ("u5", "x14", "zwwwww285"),
        ("u1930", "x14", None),
    ]
