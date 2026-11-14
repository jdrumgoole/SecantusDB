"""Gap tests of the divergences found while working RP-665 (a scalar
subquery over a domain column was typed by the domain's base):

- a CASE, COALESCE, GREATEST or LEAST over domain columns alone was typed by
  the domain's base (RP-764, fixed).

The expected answers were measured on a private, freshly initdb'd
PostgreSQL 15.19 cluster with `_SETUP`, which each statement runs after.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "drop table if exists t665 cascade",
    "drop domain if exists d665, dc665 cascade",
    "create domain d665 as int",
    "create domain dc665 as char(4)",
    "create table t665 (a d665, c dc665, b int)",
    "insert into t665 values (5, 'cd', 5)",
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


# `select_common_type` keeps the type every input shares exactly, a domain
# included: these are of the columns' domain.
COMMON = {
    "select pg_typeof(coalesce(a, a)) from t665": [("d665",)],
    "select pg_typeof(case when b > 0 then a else a end) from t665": [("d665",)],
    "select pg_typeof(greatest(a, a)) from t665": [("d665",)],
    "select pg_typeof(least(a, a)) from t665": [("d665",)],
    "select pg_typeof(coalesce(c, c)) from t665": [("dc665",)],
}


@pytest.mark.backlog_fixed("RP-764")
@pytest.mark.parametrize("query", list(COMMON))
def test_common_type_of_domain_columns_alone_is_the_domain(query: str, dsn: str) -> None:
    with _connect(dsn) as c:
        got = _answer(c, query)
    assert got == COMMON[query]
