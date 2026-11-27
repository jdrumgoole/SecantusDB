"""The order of a three-way chain's rows when the pair under the top join's
Hash is PostgreSQL's Merge Join (found while working RP-1063).

`t5 z LEFT JOIN (u1 x JOIN t4 y ON x.b = y.b) ON z.b = x.a`, all three
never analysed, is on PostgreSQL 15.20 (a private cluster, 2026-11-26) a
Hash Left Join of `z` over a Hash of the Merge Join of `x` and `y`, each
under a Sort. The pair's 42,840 rows outgrow the 8,192 buckets its
estimate of 7,200 sized (`Buckets: 65536 (originally 8192)`), and the
build's end rebuilds the chains chunk by chunk, so the 143 rows of `x.c =
'u6'`, which straddle two 32 kB chunks, come out rotated. This server once
reversed them as one table that never grew would (RP-1114): the chain's
hash joins modelled no growth, and `hash_grown_order` sized only tuples of
one fixed-length type, where these hold text.

Each run is in a database of its own, in a transaction it rolls back, so
no table is ever analysed.
"""

from __future__ import annotations

from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "create table t4 (a int, b int, c text)",
    "insert into t4 select case when g % 9 = 0 then null else g % 500 end, g % 7,"
    " 'x' || g from generate_series(1, 1000) g",
    "create table t5 (a int, b int, c text)",
    "insert into t5 select case when g % 9 = 0 then null else g % 3 end, g % 7,"
    " 'x' || g from generate_series(1, 7) g",
    "create table u1 (a int primary key, b int, c text)",
    "insert into u1 select g, g % 5, 'u' || g from generate_series(1, 300) g",
]

_QUERY = "select x.c, y.c, z.c from t5 z left join (u1 x join t4 y on x.b = y.b) on z.b = x.a"


def _rows(server: Any) -> list[tuple]:
    with psycopg.connect(_dsn(server) + " dbname=postgres", autocommit=True) as c:
        c.execute("drop database if exists rp_hashed_pair")
        c.execute("create database rp_hashed_pair")
    with psycopg.connect(_dsn(server) + " dbname=rp_hashed_pair") as c:
        try:
            for s in _SETUP:
                c.execute(s)
            return c.execute(_QUERY).fetchall()
        finally:
            c.rollback()


@pytest.mark.backlog_fixed("RP-1114")
def test_a_hashed_merge_pair_returns_a_keys_rows_in_postgres_order(rust_pg: Any) -> None:
    """The rows of `x.c = 'u6'` came out in the order PostgreSQL 15.20 returns them."""
    rows = _rows(rust_pg)
    assert len(rows) == 858
    u6 = [r[1] for r in rows if r[0] == "u6"]
    assert len(u6) == 143
    assert u6[:10] == [
        "x85",
        "x78",
        "x71",
        "x64",
        "x57",
        "x50",
        "x43",
        "x36",
        "x29",
        "x22",
    ]
