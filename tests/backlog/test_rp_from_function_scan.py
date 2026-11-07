"""Gap tests of what a user function called in FROM still gets wrong, found
while working RP-472 (2026-11-07).

- RP-473: a scan sees the rows a function the same statement ran wrote.
- RP-474 (fixed): a scalar user function in FROM was refused under an
  aggregate, an alias column list with a WHERE, and ROWS FROM.
- RP-475: EXPLAIN of a user function in FROM prints no Function Scan.
- RP-476: a function in FROM under an aggregate and a WHERE runs twice.

The expected answers were measured on a private, freshly initdb'd
PostgreSQL 15.19 cluster with `_SETUP`, which each scenario runs after.
"""

from __future__ import annotations

from collections.abc import Iterator
from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_DROP = [
    "drop function if exists fz473(), fq473()",
    "drop table if exists t473",
]

_SETUP = [
    *_DROP,
    "create table t473 (a int)",
    "insert into t473 values (1), (2), (3)",
    "create function fz473() returns setof int language sql"
    " as $$ insert into t473 values (100); select 1 $$",
    "create function fq473() returns int language sql"
    " as $$ insert into t473 values (7); select 1 $$",
]


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        for q in _DROP:
            c.execute(q)


def answer(dsn: str, call: str) -> Any:
    """The call's rows (or its error's SQLSTATE), then the table after it."""
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP:
            c.execute(q)
        try:
            rows: Any = c.execute(call).fetchall()
        except psycopg.Error as e:
            rows = e.sqlstate
        after = c.execute("select array_agg(a order by a) from t473").fetchall()
    return rows, after


# The function runs once, and its insert of 7 is not in the scan: the
# statement's snapshot was taken before the function ran.
STATEMENT_SNAPSHOT = {
    "select * from t473, fq473() order by 1": (
        [(1, 1), (2, 1), (3, 1)],
        [([1, 2, 3, 7],)],
    ),
    "select * from fq473(), t473 order by 2": (
        [(1, 1), (1, 2), (1, 3)],
        [([1, 2, 3, 7],)],
    ),
    "select * from t473 x, lateral fq473() order by 1": (
        [(1, 1), (2, 1), (3, 1)],
        [([1, 2, 3, 7],)],
    ),
    "select * from t473 where a < (select * from fq473()) + 10 order by 1": (
        [(1,), (2,), (3,)],
        [([1, 2, 3, 7],)],
    ),
}


@pytest.mark.backlog("RP-473")
@pytest.mark.parametrize("call", list(STATEMENT_SNAPSHOT))
def test_scan_does_not_see_the_functions_write(call: str, dsn: str) -> None:
    assert answer(dsn, call) == STATEMENT_SNAPSHOT[call]


SCALAR_FUNCTION_IN_FROM = {
    "select count(*) from fq473()": ([(1,)], [([1, 2, 3, 7],)]),
    "select * from fq473() as f(x) where x = 1": ([(1,)], [([1, 2, 3, 7],)]),
    "select * from rows from (fq473(), fz473())": ([(1, 1)], [([1, 2, 3, 7, 100],)]),
    "select * from fq473() f where f = 1": ([(1,)], [([1, 2, 3, 7],)]),
    "select fq473 + 1 from fq473()": ([(2,)], [([1, 2, 3, 7],)]),
}


@pytest.mark.backlog_fixed("RP-474")
@pytest.mark.parametrize("call", list(SCALAR_FUNCTION_IN_FROM))
def test_scalar_function_in_from(call: str, dsn: str) -> None:
    """A scalar function in FROM was a row source only in the shapes with a
    path of their own; it is now one row wherever a SETOF function is a
    source, and still runs once."""
    assert answer(dsn, call) == SCALAR_FUNCTION_IN_FROM[call]


# An aggregate over a WHERE with a conjunct that reads no column: each
# function still runs once, its insert in the table once.
AGGREGATE_OVER_WHERE = {
    "select count(*) from fq473() where 1 = 1": ([(1,)], [([1, 2, 3, 7],)]),
    "select count(*) from fz473() where 1 = 1": ([(1,)], [([1, 2, 3, 100],)]),
    "select max(x) from fz473() as f(x) where x > 0 and 1 = 1": (
        [(1,)],
        [([1, 2, 3, 100],)],
    ),
    "select x, count(*) from fz473() as f(x) where 1 = 1 group by x": (
        [(1, 1)],
        [([1, 2, 3, 100],)],
    ),
}


@pytest.mark.backlog("RP-476")
@pytest.mark.parametrize("call", list(AGGREGATE_OVER_WHERE))
def test_aggregate_over_where_runs_the_function_once(call: str, dsn: str) -> None:
    assert answer(dsn, call) == AGGREGATE_OVER_WHERE[call]


FUNCTION_SCAN_PLAN = {
    "explain (costs off) select * from fq473()": [("Function Scan on fq473",)],
    "explain (costs off) select * from fz473()": [("Function Scan on fz473",)],
}


@pytest.mark.backlog("RP-475")
@pytest.mark.parametrize("call", list(FUNCTION_SCAN_PLAN))
def test_function_scan_plan(call: str, dsn: str) -> None:
    rows, after = answer(dsn, call)
    assert (rows, after) == (FUNCTION_SCAN_PLAN[call], [([1, 2, 3],)])
