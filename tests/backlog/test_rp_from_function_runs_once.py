"""Regression test of RP-472: a function called in FROM ran its body twice,
so each of its writes happened twice.

Found while working RP-464. The statement's operator-type check, and the
whole-row rewrite of a FROM subquery, each planned the function only to
learn its columns, and that planning called it; a FROM subquery or CTE
over it ran it four times, WITH ORDINALITY twelve, and EXPLAIN without
ANALYZE once. The expected answers were measured on a private, freshly
initdb'd PostgreSQL 15.19 cluster with `_SETUP`, which each scenario runs
after.
"""

from __future__ import annotations

from collections.abc import Iterator
from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_DROP = [
    "drop function if exists fz(), fq(), fs472(int)",
    "drop table if exists t472",
]

_SETUP = [
    *_DROP,
    "create table t472 (a int)",
    "insert into t472 values (1), (2), (3)",
    "create function fz() returns setof int language sql"
    " as $$ insert into t472 values (100); select 1 $$",
    "create function fq() returns int language sql as $$ insert into t472 values (7); select 1 $$",
    "create function fs472(p int) returns setof int language sql"
    " as $$ update t472 set a = a + p returning a $$",
]


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        for q in _DROP:
            c.execute(q)


def answer(dsn: str, call: str) -> Any:
    """The call's rows, then the table's rows after it."""
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP:
            c.execute(q)
        cur = c.execute(call)
        rows = cur.fetchall() if cur.description else cur.statusmessage
        after = c.execute("select array_agg(a order by a) from t472").fetchall()
    return rows, after


# The table holds 1, 2, 3 before each call.
FROM_FUNCTION_RUNS_ONCE = {
    "select * from fz()": ([(1,)], [([1, 2, 3, 100],)]),
    "select * from fq()": ([(1,)], [([1, 2, 3, 7],)]),
    "select * from fs472(10) order by 1": ([(11,), (12,), (13,)], [([11, 12, 13],)]),
    "select * from (select * from fq()) s": ([(1,)], [([1, 2, 3, 7],)]),
    "with c as (select * from fz()) select * from c": ([(1,)], [([1, 2, 3, 100],)]),
    "select * from fq() with ordinality": ([(1, 1)], [([1, 2, 3, 7],)]),
    "insert into t472 select * from fq()": ("INSERT 0 1", [([1, 1, 2, 3, 7],)]),
}


@pytest.mark.backlog_fixed("RP-472")
@pytest.mark.parametrize("call", list(FROM_FUNCTION_RUNS_ONCE))
def test_from_function_runs_once(call: str, dsn: str) -> None:
    assert answer(dsn, call) == FROM_FUNCTION_RUNS_ONCE[call]


@pytest.mark.backlog_fixed("RP-472")
def test_explain_runs_nothing(dsn: str) -> None:
    """EXPLAIN without ANALYZE plans the statement and runs no function in
    it: the table is as `_SETUP` left it. (Its plan text is RP-475's.)"""
    _, after = answer(dsn, "explain (costs off) select * from fq()")
    assert after == [([1, 2, 3],)]
