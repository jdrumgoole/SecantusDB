"""Gap test of the divergence found while working RP-653 (a domain array
column in `pg_attribute`): a user column's `attndims` is always 0.

The expected answer was measured on a private, freshly initdb'd PostgreSQL
15.19 cluster; the test makes the objects it names and drops them after.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_RESET = ["drop table if exists t653n", "drop domain if exists d653n"]


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        for q in _RESET:
            c.execute(q)


@pytest.mark.backlog("RP-658")
def test_array_column_attndims(dsn: str) -> None:
    """`attndims` is the number of dimensions the column was DECLARED with
    (`ColumnDef`'s `arrayBounds`), a domain's array included; a size in the
    brackets counts the same."""
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in [*_RESET, "create domain d653n as int"]:
            c.execute(q)
        c.execute("create table t653n (a int[], b int[][], c int, d int[3], e d653n[])")
        got = c.execute(
            "select attname, attndims from pg_attribute "
            "where attrelid = 't653n'::regclass and attnum > 0 order by attnum"
        ).fetchall()
    assert got == [("a", 1), ("b", 2), ("c", 0), ("d", 1), ("e", 1)]
