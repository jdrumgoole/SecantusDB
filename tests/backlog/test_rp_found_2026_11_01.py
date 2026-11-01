"""Divergences found on 2026-11-01, each measured on PostgreSQL 15.19 with
the same statements in a rolled-back transaction.

RP-315 was found while sizing `tsvector` datums for RP-287.
"""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.conftest import pg_answer


@pytest.mark.backlog("RP-315")
def test_a_grouped_column_size_is_the_stored_one(pg: Any) -> None:
    """`pg_column_size` of a table column inside an aggregate reads the
    stored datum (a short varlena header) on PostgreSQL 15.19: 35 for this
    `tsvector`, 25 for this `jsonb`; here the in-memory size, 38 and 28, as text."""
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
