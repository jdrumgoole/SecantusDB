"""Gap tests of the divergences found while working RP-655 (a cast of a
domain column to its own domain checks nothing):

- a write re-checked a domain column's value that was not coerced into the
  domain -- a column an UPDATE does not assign, or a value already of the
  column's domain -- so a row stored before a `NOT VALID` CHECK could not be
  updated or copied (RP-664, fixed; more shapes in
  `test_rp_found_rp664.py`);
- a scalar subquery over a domain column was typed by the domain's base
  (RP-665, fixed): it is of the domain, and the CASE / COALESCE / GREATEST /
  NULLIF over it of the domain's base unless every input is the domain.

The expected answers were measured on a private, freshly initdb'd
PostgreSQL 15.19 cluster with `_SETUP`, which each statement runs after.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "drop table if exists t655, t665, tc665 cascade",
    "drop domain if exists d655, dn665, dc665 cascade",
    "create domain d655 as int",
    "create domain dn665 as int not null",
    "create domain dc665 as char(4)",
    "create table t655 (a d655, b int)",
    "insert into t655 values (-1, -1), (5, 5)",
    "create table t665 (a d655, b int, n dn665, c dc665)",
    "insert into t665 values (-1, -1, 1, 'ab'), (5, 5, 2, 'cd')",
    "create table tc665 (k int)",
    "insert into tc665 values (-1), (5), (7)",
    "alter domain d655 add constraint d655_pos check (value > 0) not valid",
]


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        for q in _SETUP[:2]:
            c.execute(q)


def _connect(dsn: str) -> psycopg.Connection:
    c = psycopg.connect(dsn, autocommit=True)
    for q in _SETUP:
        c.execute(q)
    return c


def _answer(c: psycopg.Connection, query: str) -> object:
    try:
        cur = c.execute(query)
    except psycopg.Error as e:
        return ("error", e.sqlstate, str(e).splitlines()[0])
    return cur.fetchall() if cur.description else cur.statusmessage


# `ExecUpdate` stores the columns an UPDATE does not assign as they are, and
# an assigned value already of the column's domain is no coercion
# (`coerce_to_target_type` returns it as it is): neither checks the domain,
# so the row stored before the NOT VALID CHECK can still be written.
WRITES = {
    "update t655 set b = b where b < 0": "UPDATE 1",
    "update t655 set b = a::d655 where a::d655 < 0 returning a, b": [(-1, -1)],
    "update t655 set a = a where b < 0": "UPDATE 1",
    "insert into t655 select a, 9 from t655 where b < 0": "INSERT 0 1",
    # A new value is coerced into the domain, and checked.
    "update t655 set a = -5 where b = 5": (
        "error",
        "23514",
        'value for domain d655 violates check constraint "d655_pos"',
    ),
}


@pytest.mark.backlog_fixed("RP-664")
@pytest.mark.parametrize("query", list(WRITES))
def test_write_checks_only_what_it_coerces_into_a_domain(query: str, dsn: str) -> None:
    with _connect(dsn) as c:
        got = _answer(c, query)
    assert got == WRITES[query]


# A scalar subquery's type is its one column's (`exprType` of the SubLink):
# over a domain column, the domain.
SCALAR = {
    "select pg_typeof((select a from t655 where b < 0))": [("d655",)],
    "select pg_typeof((select a from t655 where b > 0))": [("d655",)],
}


@pytest.mark.backlog_fixed("RP-665")
@pytest.mark.parametrize("query", list(SCALAR))
def test_scalar_subquery_over_a_domain_column_is_of_the_domain(query: str, dsn: str) -> None:
    with _connect(dsn) as c:
        got = _answer(c, query)
    assert got == SCALAR[query]


# Around RP-665, on the same 15.19 cluster. No row is a NULL of the domain,
# checked against none of its constraints (a NOT NULL one included); a cast
# of the subquery to its own domain is no coercion; and the domain shows
# wherever the column reaches it -- a set operation, a correlated subquery, a
# FROM subquery, a WITH item, a join, an `ARRAY[...]`. `select_common_type`
# keeps a domain only when every input is exactly that type -- a CASE's
# missing ELSE is an untyped NULL -- and otherwise resolves each by its base;
# NULLIF is typed by its `=`'s left input, the base's.
AROUND = {
    "select pg_typeof((select a from t665 where false))": [("d655",)],
    "select (select a from t665 where false) is null": [(True,)],
    "select pg_typeof((select n from t665 where false))": [("dn665",)],
    "select (select n from t665 where false)": [(None,)],
    "select pg_typeof((select c from t665 where b > 0))": [("dc665",)],
    "select (select c from t665 where b > 0) || '|'": [("cd|",)],
    "select octet_length((select c from t665 where b > 0))": [(4,)],
    "select length((select c from t665 where b > 0))": [(2,)],
    "select (select a from t665 where b < 0) + 1": [(0,)],
    "select pg_typeof((select a from t665 where b < 0) + 1)": [("integer",)],
    "select (select a from t665 where b < 0)::d655": [(-1,)],
    "select pg_typeof((select a::int from t665 where b < 0))": [("integer",)],
    "select pg_typeof((select max(a) from t665))": [("integer",)],
    "select pg_typeof((select a from t665 where b < 0"
    " union select a from t665 where b > 0 limit 1))": [("d655",)],
    "select k, pg_typeof((select a from t665 where b = k)) from tc665 order by k": [
        (-1, "d655"),
        (5, "d655"),
        (7, "d655"),
    ],
    "select k, (select a from t665 where b = k) from tc665 order by k": [
        (-1, -1),
        (5, 5),
        (7, None),
    ],
    "select k from tc665 where k = (select a from t665 where b < 0)": [(-1,)],
    "select pg_typeof((select x from (select a as x from t665) s where x < 0))": [("d655",)],
    "with w as (select a from t665) select pg_typeof((select a from w limit 1))": [("d655",)],
    "select pg_typeof((select t.a from t665 t join tc665 on k = b where b < 0))": [("d655",)],
    "select pg_typeof(array[(select a from t665 where b < 0)])": [("d655[]",)],
    "select pg_typeof(coalesce((select a from t665 where b < 0), 0))": [("integer",)],
    "select pg_typeof(coalesce((select a from t665 where b < 0),"
    " (select a from t665 where b > 0)))": [("d655",)],
    "select case when true then (select a from t665 where b < 0) end": [(-1,)],
    "select pg_typeof(case when true then (select a from t665 where b < 0) end)": [("integer",)],
    "select pg_typeof(case when true then (select a from t665 where b < 0)"
    " else (select a from t665 where b > 0) end)": [("d655",)],
    "select pg_typeof(greatest((select a from t665 where b < 0), 0))": [("integer",)],
    "select pg_typeof(nullif((select a from t665 where b < 0), 0))": [("integer",)],
    "select pg_typeof(coalesce('5'::d655, 0))": [("integer",)],
    "select pg_typeof(case when true then '5'::d655 end)": [("integer",)],
    "select pg_typeof(coalesce(a, 0)) from t665 where b > 0": [("integer",)],
    "select pg_typeof(case when true then a end) from t665 where b > 0": [("integer",)],
    "select (select a from t665 where b < 0) = any(array[-1, 2])": [(True,)],
}


@pytest.mark.backlog_fixed("RP-665")
@pytest.mark.parametrize("query", list(AROUND))
def test_scalar_subquery_over_a_domain_column_around(query: str, dsn: str) -> None:
    with _connect(dsn) as c:
        got = _answer(c, query)
    assert got == AROUND[query]


# A domain is sent as its base type (`SendRowDescriptionMessage`), so the
# subquery's column describes as `int4` (15.19).
@pytest.mark.backlog_fixed("RP-665")
@pytest.mark.parametrize(
    "query",
    ["select (select a from t665 where b < 0)", "select (select n from t665 where false)"],
)
def test_scalar_subquery_over_a_domain_column_describes_its_base(query: str, dsn: str) -> None:
    with _connect(dsn) as c:
        cur = c.execute(query)
        assert [d.type_code for d in cur.description] == [23]
