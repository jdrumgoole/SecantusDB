"""Gap tests of what a user function called in FROM still gets wrong, found
while working RP-472 (2026-11-07).

- RP-473 (fixed): a scan saw the rows a function the same statement ran
  wrote.
- RP-474 (fixed): a scalar user function in FROM was refused under an
  aggregate, an alias column list with a WHERE, and ROWS FROM.
- RP-475: EXPLAIN of a user function in FROM prints no Function Scan.
- RP-476 (fixed): a function in FROM under an aggregate and a WHERE ran
  twice.
- RP-496 (fixed): a volatile function in a FROM subquery or a CTE joined
  to a table ran twice (found while working RP-473).

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
    "drop function if exists fz473(), fq473(), fu473(), fd473(), fs473(), fn473(),"
    " fp473(), fx473()",
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
    "create function fu473() returns int language sql"
    " as $$ update t473 set a = a * 10 where a = 2; select 1 $$",
    "create function fd473() returns int language sql"
    " as $$ delete from t473 where a = 3; select 1 $$",
    "create function fs473() returns bigint language sql"
    " as $$ insert into t473 values (9); select count(*) from t473 $$",
    "create function fn473() returns bigint language sql"
    " as $$ select count(*) from t473, fq473() $$",
    "create function fp473() returns bigint language plpgsql as $f$ begin"
    " insert into t473 values (8); return (select count(*) from t473); end $f$",
    "create function fx473() returns int language plpgsql as $f$ begin"
    " begin insert into t473 values (5); perform 1 / 0;"
    " exception when division_by_zero then null; end;"
    " insert into t473 values (6); return 1; end $f$",
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


@pytest.mark.backlog_fixed("RP-473")
@pytest.mark.parametrize("call", list(STATEMENT_SNAPSHOT))
def test_scan_does_not_see_the_functions_write(call: str, dsn: str) -> None:
    """The scan saw the function's insert, as the function ran while the
    statement was planned; the statement now reads its tables as they were
    before its calls wrote them."""
    assert answer(dsn, call) == STATEMENT_SNAPSHOT[call]


def answers(dsn: str, steps: tuple[str, ...]) -> list[Any]:
    """The rows of each step that returns rows, then the table after them."""
    out: list[Any] = []
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP:
            c.execute(q)
        for q in steps:
            cur = c.execute(q)
            if cur.description:
                out.append(cur.fetchall())
        out.append(c.execute("select array_agg(a order by a) from t473").fetchall())
    return out


# What a function updates or deletes is still seen as it was; the body's
# own statements see what the body wrote before them, but not what a call
# of their own writes; a savepoint rolls the call's writes back as before.
CALLS_WRITES = {
    ("select * from t473, fu473() order by 1",): [
        [(1, 1), (2, 1), (3, 1)],
        [([1, 3, 20],)],
    ],
    ("select * from t473, fd473() order by 1",): [
        [(1, 1), (2, 1), (3, 1)],
        [([1, 2],)],
    ],
    ("select fs473()",): [[(4,)], [([1, 2, 3, 9],)]],
    ("select fn473()",): [[(3,)], [([1, 2, 3, 7],)]],
    ("select * from t473, fp473() order by 1",): [
        [(1, 4), (2, 4), (3, 4)],
        [([1, 2, 3, 8],)],
    ],
    ("select * from t473, fx473() order by 1",): [
        [(1, 1), (2, 1), (3, 1)],
        [([1, 2, 3, 6],)],
    ],
    ("update t473 set a = a + fq473()",): [[([2, 3, 4, 7, 7, 7],)]],
    (
        "begin",
        "insert into t473 values (40)",
        "select * from t473, fd473() order by 1",
        "select * from t473, fq473() order by 1",
        "select * from t473 order by 1",
        "commit",
    ): [
        [(1, 1), (2, 1), (3, 1), (40, 1)],
        [(1, 1), (2, 1), (40, 1)],
        [(1,), (2,), (7,), (40,)],
        [([1, 2, 7, 40],)],
    ],
    (
        "begin",
        "savepoint s",
        "select * from t473, fu473() order by 1",
        "rollback to s",
        "commit",
    ): [[(1, 1), (2, 1), (3, 1)], [([1, 2, 3],)]],
}


@pytest.mark.backlog_fixed("RP-473")
@pytest.mark.parametrize("steps", list(CALLS_WRITES), ids=lambda s: " / ".join(s))
def test_statement_reads_rows_as_before_its_calls(steps: tuple[str, ...], dsn: str) -> None:
    """A row a function updated was seen at its new value, a row it deleted
    was gone: each is now seen as it was before the call, while the next
    statement sees the call's writes."""
    assert answers(dsn, steps) == CALLS_WRITES[steps]


# A volatile function in a FROM subquery or a CTE joined to a table runs
# once on PostgreSQL 15.19: one 7 inserted.
JOINED_SUBQUERY_RUNS_ONCE = {
    "with w as (select fq473()) select * from t473, w order by 1": (
        [(1, 1), (2, 1), (3, 1)],
        [([1, 2, 3, 7],)],
    ),
    "with w as materialized (select fq473()) select * from t473, w order by 1": (
        [(1, 1), (2, 1), (3, 1)],
        [([1, 2, 3, 7],)],
    ),
    "with w as (select * from fq473()) select * from t473, w order by 1": (
        [(1, 1), (2, 1), (3, 1)],
        [([1, 2, 3, 7],)],
    ),
    "select * from t473, (select fq473()) s order by 1": (
        [(1, 1), (2, 1), (3, 1)],
        [([1, 2, 3, 7],)],
    ),
    "select * from t473 join (select fq473() as x) s on t473.a = s.x order by 1": (
        [(1, 1)],
        [([1, 2, 3, 7],)],
    ),
    "select * from t473 left join (select fq473() as x) s on t473.a = s.x order by 1": (
        [(1, 1), (2, None), (3, None)],
        [([1, 2, 3, 7],)],
    ),
    "select * from (select fq473() as x) s join t473 on true order by 2": (
        [(1, 1), (1, 2), (1, 3)],
        [([1, 2, 3, 7],)],
    ),
    "select count(*) from t473, (select fq473()) s": ([(3,)], [([1, 2, 3, 7],)]),
    "select * from t473 a, t473 b, (select fq473()) s order by 1, 2": (
        [(1, 1, 1), (1, 2, 1), (1, 3, 1), (2, 1, 1), (2, 2, 1), (2, 3, 1)]
        + [(3, 1, 1), (3, 2, 1), (3, 3, 1)],
        [([1, 2, 3, 7],)],
    ),
}


@pytest.mark.backlog_fixed("RP-496")
@pytest.mark.parametrize("call", list(JOINED_SUBQUERY_RUNS_ONCE))
def test_joined_subquery_runs_its_function_once(call: str, dsn: str) -> None:
    """The narrow two-table join path planned the subquery side, running
    its function, before it refused the shape, and the general join planner
    then ran it again; the narrow attempt is now made with calls switched
    off first."""
    assert answer(dsn, call) == JOINED_SUBQUERY_RUNS_ONCE[call]


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
    # The WHERE's rewrite puts the function in a FROM subquery, and the
    # grouped expression's rewrite plans that subquery again.
    "select x + 1, count(*) from fz473() as f(x) where 1 = 1 group by x": (
        [(2, 1)],
        [([1, 2, 3, 100],)],
    ),
    "with c as (select * from fz473() f(x)) select x + 1, count(*) from c where 1 = 1 group by x": (
        [(2, 1)],
        [([1, 2, 3, 100],)],
    ),
}


@pytest.mark.backlog_fixed("RP-476")
@pytest.mark.parametrize("call", list(AGGREGATE_OVER_WHERE))
def test_aggregate_over_where_runs_the_function_once(call: str, dsn: str) -> None:
    """An aggregate's first plan, refused, ran the FROM function before a
    rewrite planned the statement again and ran it a second time; that
    first attempt is now made with calls switched off."""
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
