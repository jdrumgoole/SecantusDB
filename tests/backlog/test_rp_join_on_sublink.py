"""Regression test of a subquery in a JOIN's ON (RP-438, found while working
RP-437).

PostgreSQL evaluates a scalar or IN subquery in a join condition as it does
one in a WHERE. The ON with a SubLink in it was refused with 0A000 until
RP-438 resolved the subqueries in a join's ON as it does a WHERE's. The rows
were measured on PostgreSQL 15.19 over the same rows as `VALUES` lists.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "drop table if exists js, jk",
    "create table js (a int, c text)",
    "create table jk (a int, b text, bc int)",
    "insert into js values (1, 'u'), (2, 'v'), (3, 'w'), (5, 'z')",
    "insert into jk values (1, 'p', 10), (2, 'p', 20), (2, 'q', 30), (3, null, 40), (5, 'r', 50)",
]

ON_SUBLINK = {
    "select js.a, jk.bc from js join jk"
    " on js.a = jk.a and jk.b = (select 'p'::text) order by 1, 2": [
        (1, 10),
        (2, 20),
    ],
    "select js.a, jk.bc from js left join jk"
    " on js.a = jk.a and jk.b = (select 'p'::text) order by 1, 2": [
        (1, 10),
        (2, 20),
        (3, None),
        (5, None),
    ],
    "select js.a from js join jk on js.a = jk.a and jk.a in (select 2) order by 1": [(2,), (2,)],
}


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    dsn = _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP:
            c.execute(q)
    yield dsn
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute(_SETUP[0])


@pytest.mark.backlog_fixed("RP-438")
@pytest.mark.parametrize("query", list(ON_SUBLINK))
def test_subquery_in_join_on(query: str, dsn: str) -> None:
    """A scalar or IN subquery in a join's ON is evaluated, under an inner
    and a LEFT join, as PostgreSQL 15.19 does; it was 0A000."""
    with psycopg.connect(dsn, autocommit=True) as c:
        try:
            rows: object = c.execute(query).fetchall()
        except psycopg.Error as e:
            rows = (e.sqlstate, e.diag.message_primary)
    assert rows == ON_SUBLINK[query]
