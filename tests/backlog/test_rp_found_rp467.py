"""Gap tests of the divergences found while working RP-467 (`pg_get_viewdef`
of a join names its columns as ruleutils does): a view whose `*` covers a
table with an alias column list.

An answer is `(column names, rows)`, or `(sqlstate, message)` for an
error. The expected answers were measured on a private, freshly initdb'd
PostgreSQL 15.19 cluster after `_SETUP`.
"""

from __future__ import annotations

from collections.abc import Iterator
from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "drop table if exists fs cascade",
    "create table fs (a int, c text)",
    "insert into fs values (1, 'y')",
]


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        c.execute(_SETUP[0])


def view_answer(dsn: str, query: str) -> Any:
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP:
            c.execute(q)
        try:
            c.execute("create view rp_sv as " + query)
            cur = c.execute("select * from rp_sv")
            return ([d.name for d in cur.description or []], cur.fetchall())
        except psycopg.Error as e:
            return (e.sqlstate, e.diag.message_primary)
        finally:
            c.execute("drop view if exists rp_sv")


# PostgreSQL expands a view's `*` into the columns as the alias list names
# them, so the view reads `x, y` (or `x, c` under a shorter list). Here
# CREATE VIEW's star expansion (`view_deps::expand_stars`) writes the
# table's own names, `f.a, f.c`, and every read of the view is 42703.
ALIASED_TABLE_STAR = {
    "select * from fs f(x, y)": (["x", "y"], [(1, "y")]),
    "select * from fs f(x)": (["x", "c"], [(1, "y")]),
    "select f.* from fs f(x)": (["x", "c"], [(1, "y")]),
}


@pytest.mark.backlog("RP-512")
@pytest.mark.parametrize("query", list(ALIASED_TABLE_STAR))
def test_view_star_over_an_aliased_column_list(query: str, dsn: str) -> None:
    assert view_answer(dsn, query) == ALIASED_TABLE_STAR[query]
