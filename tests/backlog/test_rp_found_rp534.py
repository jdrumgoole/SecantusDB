"""Gap tests of the divergences found while working RP-534 (a composite
value's IS NULL in a WHERE or HAVING): a domain over a table's row type.

An answer is the rows, or `(sqlstate, message, position)` for an error.
The expected answers were measured on a private, freshly initdb'd
PostgreSQL 15.19 cluster with `_SETUP`, which each statement runs after.
"""

from __future__ import annotations

from collections.abc import Iterator
from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "drop table if exists fs, fd cascade",
    "drop domain if exists dfs",
    "create table fs (a int, c text)",
    "create domain dfs as fs",
    "create table fd (k dfs)",
    "insert into fd values (row(null, null)), (row(1, null)), (row(1, 'a')), (null)",
]


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        c.execute(_SETUP[0])
        c.execute(_SETUP[1])


def answer(dsn: str, query: str) -> Any:
    with psycopg.connect(dsn, autocommit=True) as c:
        try:
            for q in _SETUP:
                c.execute(q)
            cur = c.execute(query)
        except psycopg.Error as e:
            return (e.sqlstate, e.diag.message_primary, e.diag.statement_position)
        return cur.fetchall()


# A domain may be over a composite type (PostgreSQL 11 on), a table's row
# type included, and its values are rows: `type_is_rowtype` is true of the
# domain, so a null test over a column of it reads the fields, as it does
# over the composite itself. Here `create domain dfs as fs` is 42704 `type
# "fs" does not exist`.
DOMAIN_OVER_COMPOSITE = {
    "select k is null, k is not null from fd": [
        (True, False),
        (False, False),
        (False, True),
        (True, False),
    ],
    "select count(*) from fd where k is not null": [(1,)],
    "select count(*) from fd where k is null": [(2,)],
}


@pytest.mark.backlog("RP-538")
@pytest.mark.parametrize("query", list(DOMAIN_OVER_COMPOSITE))
def test_domain_over_a_table_row_type(query: str, dsn: str) -> None:
    assert answer(dsn, query) == DOMAIN_OVER_COMPOSITE[query]
