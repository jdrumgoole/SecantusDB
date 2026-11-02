"""Divergences found on 2026-11-01, each measured on PostgreSQL 15.19 with
the same statements in a rolled-back transaction.

RP-315 was found while sizing `tsvector` datums for RP-287.
"""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.conftest import pg_answer


@pytest.mark.backlog_fixed("RP-315")
def test_a_grouped_column_size_is_the_stored_one(pg: Any) -> None:
    """`pg_column_size` of a table column inside an aggregate reads the
    stored datum (a short varlena header) on PostgreSQL 15.19: 35 for this
    `tsvector`, 25 for this `jsonb`. The server gave the in-memory size, 38
    and 28, as text, until the grouped path kept the column's provenance."""
    pg.execute("begin")
    try:
        for s in [
            "create table rp315_t (v tsvector, j jsonb)",
            "insert into rp315_t values ('w1:1 y1:2 z1:3', '{\"a\": 1}')",
        ]:
            got = pg_answer(pg, s)
            assert got[0] == "ok", got
        got = pg_answer(
            pg,
            "select max(pg_column_size(v)), pg_column_size(j) from rp315_t group by 2",
        )
    finally:
        pg.execute("rollback")
    assert got == ("ok", [(35, 25)])


@pytest.mark.backlog_fixed("RP-315")
def test_grouped_column_sizes_are_stored_and_int4(pg: Any) -> None:
    """The other grouped shapes of RP-315, as PostgreSQL 15.19 answers them:
    the stored size, typed `int4` (oid 23), whether the call is the key, an
    aggregate's argument or under an operator."""
    pg.execute("begin")
    try:
        for s in [
            "create table rp315_s (v tsvector, j jsonb)",
            "insert into rp315_s values ('w1:1 y1:2 z1:3', '{\"a\": 1}')",
        ]:
            got = pg_answer(pg, s)
            assert got[0] == "ok", got
        got = {}
        for q, want in [
            ("select pg_column_size(j) from rp315_s group by 1", [(25,)]),
            ("select max(pg_column_size(v)) from rp315_s", [(35,)]),
            (
                "select count(*), pg_column_size(j) from rp315_s group by pg_column_size(j)",
                [(1, 25)],
            ),
            ("select pg_column_size(j) + 0 from rp315_s group by 1", [(25,)]),
        ]:
            cur = pg.execute(q)
            got[q] = (cur.fetchall(), cur.description[-1].type_code)
            assert got[q] == (want, 23), (q, got[q])
    finally:
        pg.execute("rollback")
