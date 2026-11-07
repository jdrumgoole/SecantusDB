"""Gap tests of a correlated subquery's output, found while working RP-445.

The column name of an expression target (RP-449) was measured on PostgreSQL
15.19 and needs no table. The sequence draws (RP-450, fixed) were measured on a
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
}


@pytest.mark.backlog("RP-449")
@pytest.mark.parametrize("query", list(NAMED))
def test_expression_target_is_named_column(query: str, rust_pg: Any) -> None:
    """Named `\\x1fcorrelated`, the internal per-row call, here."""
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        cur = c.execute(query)
        assert cur.description is not None
        got = (cur.description[0].name, cur.fetchone()[0])  # type: ignore[index]
    assert got == NAMED[query]


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
