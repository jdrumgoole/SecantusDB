"""Gap tests of a correlated subquery's output, found while working RP-445.

The column name of an expression target (RP-449, fixed) was measured on
PostgreSQL 15.19 and needs no table; RETURNING's, which needs one, on a
private, freshly initdb'd 15.19 cluster. The sequence draws (RP-450, fixed) were measured on a
private, freshly initdb'd PostgreSQL 15.19 cluster, as a sequence is an
object the shared reference must not be given.
"""

from __future__ import annotations

from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

NAMED = {
    "select (select a + 1) from (select 1 a) t": ("?column?", 2),
    "select (select (select a) + 1) from (select 1 a) t": ("?column?", 2),
    "select (select a + 1 from (select 2 b) u) from (select 1 a) t": ("?column?", 2),
    # A scalar subquery names its column through a cast (FigureColname
    # strength 2), correlated or not.
    "select (select a + 1)::int from (select 1 a) t": ("?column?", 2),
    "select (select a)::int from (select 1 a) t": ("a", 1),
    "select (select abs(a))::text from (select 1 a) t": ("abs", "1"),
    "select (select 1 + 1)::int": ("?column?", 2),
    "select (select 1 as x)::text": ("x", "1"),
    "select exists(select 1)::int": ("exists", 1),
}


@pytest.mark.backlog_fixed("RP-449")
@pytest.mark.parametrize("query", list(NAMED))
def test_expression_target_is_named_column(query: str, rust_pg: Any) -> None:
    """Was named `\\x1fcorrelated`, the internal per-row call (and a cast
    over an uncorrelated one its type's name, `int4`): a target whose
    subquery was replaced kept only names other than `?column?`."""
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        cur = c.execute(query)
        assert cur.description is not None
        got = (cur.description[0].name, cur.fetchone()[0])  # type: ignore[index]
    assert got == NAMED[query]


# Each statement's RETURNING column names and rows, on PostgreSQL 15.19
# from a fresh `r(a int, b text)`, in this order.
RETURNED = [
    ("insert into rp449_r values (1, 'x') returning (select a + 1)", ["?column?"], [(2,)]),
    (
        "insert into rp449_r values (2, 'y') returning (select a)::text, (select 1 + 1)::int",
        ["a", "?column?"],
        [("2", 2)],
    ),
    ("update rp449_r set b = 'z' where a = 2 returning (select a + 1)::int", ["?column?"], [(3,)]),
    (
        "delete from rp449_r where a = 1 returning (select a + 1), exists(select a)",
        ["?column?", "exists"],
        [(2, True)],
    ),
]


@pytest.mark.backlog_fixed("RP-449")
def test_returning_subquery_is_named_column(rust_pg: Any) -> None:
    """Was named `\\x1fcorrelated` in an INSERT's, an UPDATE's and a
    DELETE's RETURNING, which named a target from its rewritten value."""
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        c.execute("drop table if exists rp449_r")
        c.execute("create table rp449_r(a int, b text)")
        got = []
        for q, _, _ in RETURNED:
            cur = c.execute(q)
            assert cur.description is not None
            got.append(([d.name for d in cur.description], cur.fetchall()))
        c.execute("drop table rp449_r")
    assert got == [(names, rows) for _, names, rows in RETURNED]


DRAWS = [
    "select (select nextval('{q}') + a) from (values (1), (2)) t(a)",
    "select (select (select nextval('{q}') + a)) from (values (1), (2)) t(a)",
]


@pytest.mark.backlog_fixed("RP-450")
@pytest.mark.parametrize("query", DRAWS)
def test_fromless_correlated_nextval_draws_once_per_row(query: str, rust_pg: Any) -> None:
    """Was 3 and 5, and a next draw of 4: the plan that only checks and
    types a FROM-less body evaluated its targets, drawing once more than
    the rows it ran for."""
    q = f"rp450_{DRAWS.index(query)}"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        c.execute(f"drop sequence if exists {q}")
        c.execute(f"create sequence {q}")
        try:
            rows = c.execute(query.format(q=q)).fetchall()
            following = c.execute(f"select nextval('{q}')").fetchone()
        finally:
            c.execute(f"drop sequence {q}")
    assert (rows, following) == ([(2,), (4,)], (3,))
