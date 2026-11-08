"""Gap tests of what was found while working RP-527: RP-545 (a FROM item
naming no relation is reported after the statement's qualified column
references) and RP-546 (a function over a table's row type does not
depend on the table).

An answer is the rows, `"ok"` for a statement with none, or `(sqlstate,
message, detail, position)` for an error. The expected answers were
measured on a private, freshly initdb'd PostgreSQL 15.19 cluster.
"""

from __future__ import annotations

from collections.abc import Iterator
from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"


def _run(c: psycopg.Connection, query: str) -> Any:
    try:
        cur = c.execute(query)
    except psycopg.Error as e:
        return (
            e.sqlstate,
            e.diag.message_primary,
            e.diag.message_detail,
            e.diag.statement_position,
        )
    return cur.fetchall() if cur.description else "ok"


# `transformFromClause` runs before the select list and WHERE are read, so
# a FROM relation that does not exist is 42P01 `relation "nosuch" does not
# exist` at its name, whatever the qualified references say. Here the
# qualifier is checked first: `fs.c` is `missing FROM-clause entry`, and
# `nosuch.c` has the right message at the reference.
MISSING_FROM_RELATION = {
    "select fs.c from nosuch": ("42P01", 'relation "nosuch" does not exist', None, "18"),
    "select nosuch.c from nosuch": ("42P01", 'relation "nosuch" does not exist', None, "22"),
    "select a from nosuch where zz.a = 1": (
        "42P01",
        'relation "nosuch" does not exist',
        None,
        "15",
    ),
}


@pytest.mark.backlog("RP-545")
@pytest.mark.parametrize("query", list(MISSING_FROM_RELATION))
def test_missing_from_relation_reported_first(query: str, dsn: str) -> None:
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute("drop table if exists nosuch")
        assert _run(c, query) == MISSING_FROM_RELATION[query]


# A function whose argument is a table's row type depends on that type
# (`pg_depend`), so a plain DROP TABLE is 2BP01 and CASCADE drops the
# function too. Here the DROP succeeds and the function is left over a
# type that no longer exists.
@pytest.mark.backlog("RP-546")
def test_row_type_function_depends_on_its_table(dsn: str) -> None:
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute("drop table if exists rq534 cascade")
        c.execute("drop function if exists rq534f(int)")
        c.execute("create table rq534 (a int, c text)")
        c.execute(
            "create or replace function rq534f(r rq534) returns text"
            " language sql as $$ select r.c $$"
        )
        try:
            got = [
                _run(c, "drop table rq534"),
                _run(c, "drop table rq534 cascade"),
                _run(c, "select count(*) from pg_proc where proname = 'rq534f'"),
            ]
        finally:
            c.execute("drop table if exists rq534 cascade")
    assert got == [
        (
            "2BP01",
            "cannot drop table rq534 because other objects depend on it",
            "function rq534f(rq534) depends on type rq534",
            None,
        ),
        "ok",
        [(0,)],
    ]
