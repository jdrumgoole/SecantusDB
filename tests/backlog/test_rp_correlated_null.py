"""Gap test of a correlated EXISTS over a join whose WHERE compares an inner
column to a NULL outer value (found while working RP-430).

`k.b = e.b` with `e.b` NULL is NULL in PostgreSQL, so it matches no row,
however many NULLs `k.b` holds. Here the EXISTS finds `k`'s NULL `b` and
answers true. The rows were measured on a private, freshly initdb'd
PostgreSQL 15.19 cluster with `_SETUP`'s tables.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "drop table if exists ne, ns, nk",
    "create table ne (a int, b text)",
    "create table ns (a int, c text)",
    "create table nk (a int, b text, bc int)",
    "insert into ne values (1, 'p'), (2, 'q'), (3, null), (4, 'r'), (null, 'p')",
    "insert into ns values (1, 'u'), (2, 'v'), (3, 'w'), (5, 'z')",
    "insert into nk values (1, 'p', 10), (2, 'p', 20), (2, 'q', 30), (3, null, 40), (5, 'r', 50)",
]

# The outer row (3, NULL) is not among them.
_PQR = [(1, "p"), (2, "q"), (4, "r"), (None, "p")]

EXISTS_OVER_JOIN = {
    "select * from ne where exists (select 1 from ns join nk on ns.a = nk.a"
    " where nk.b = ne.b) order by 1, 2": _PQR,
    "select * from ne where exists (select 1 from ns left join nk on ns.a = nk.a"
    " where nk.b = ne.b) order by 1, 2": _PQR,
    "select * from ne where exists (select 1 from ns join nk on ns.a = nk.a"
    " where nk.b = ne.b || '') order by 1, 2": _PQR,
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


@pytest.mark.backlog("RP-437")
@pytest.mark.parametrize("query", list(EXISTS_OVER_JOIN))
def test_correlated_exists_over_join_null_matches_nothing(query: str, dsn: str) -> None:
    """A NULL outer value compared with `=` in a correlated EXISTS over a
    join matches no inner row, NULL or not. PostgreSQL 15.19 leaves the
    outer row (3, NULL) out; here it is in."""
    with psycopg.connect(dsn, autocommit=True) as c:
        try:
            rows: object = c.execute(query).fetchall()
        except psycopg.Error as e:
            rows = (e.sqlstate, e.diag.message_primary)
    assert rows == EXISTS_OVER_JOIN[query]
