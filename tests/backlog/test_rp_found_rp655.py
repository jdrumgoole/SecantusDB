"""Gap tests of the divergences found while working RP-655 (a cast of a
domain column to its own domain checks nothing):

- a write re-checks a domain column's value that is not coerced into the
  domain -- a column an UPDATE does not assign, or a value already of the
  column's domain -- so a row stored before a `NOT VALID` CHECK cannot be
  updated or copied;
- a scalar subquery over a domain column is typed by the domain's base.

The expected answers were measured on a private, freshly initdb'd
PostgreSQL 15.19 cluster with `_SETUP`, which each statement runs after.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "drop table if exists t655 cascade",
    "drop domain if exists d655 cascade",
    "create domain d655 as int",
    "create table t655 (a d655, b int)",
    "insert into t655 values (-1, -1), (5, 5)",
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


@pytest.mark.backlog("RP-664")
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


@pytest.mark.backlog("RP-665")
@pytest.mark.parametrize("query", list(SCALAR))
def test_scalar_subquery_over_a_domain_column_is_of_the_domain(query: str, dsn: str) -> None:
    with _connect(dsn) as c:
        got = _answer(c, query)
    assert got == SCALAR[query]
