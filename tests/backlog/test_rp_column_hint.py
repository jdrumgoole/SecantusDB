"""The gap test of RP-408: a column that does not exist is a 42703 whose
HINT names the closest column of the FROM (`errorMissingColumn` over
`searchRangeTableForCol`, a Levenshtein match), and this server sends no
HINT. Found while working RP-405.

An answer is `(sqlstate, message, hint)`. The expected answers were
measured on a private, freshly initdb'd PostgreSQL 15.19 cluster.
"""

from __future__ import annotations

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "create table e (a int, b text)",
    "create table s (a int, c text)",
]

SCENARIOS = {
    # A qualified name whose column only the other table has.
    "select s.b from e, s": (
        "42703",
        "column s.b does not exist",
        'Perhaps you meant to reference the column "e.b".',
    ),
    "select e.c from e, s": (
        "42703",
        "column e.c does not exist",
        'Perhaps you meant to reference the column "s.c".',
    ),
    # A misspelt name, bare or qualified.
    "select bb from e": (
        "42703",
        'column "bb" does not exist',
        'Perhaps you meant to reference the column "e.b".',
    ),
    "select e.bb from e": (
        "42703",
        "column e.bb does not exist",
        'Perhaps you meant to reference the column "e.b".',
    ),
}


def answer(dsn: str, query: str) -> tuple[str | None, str | None, str | None]:
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute("drop table if exists e, s")
        for q in _SETUP:
            c.execute(q)
        try:
            c.execute(query)
        except psycopg.Error as e:
            return (e.sqlstate, e.diag.message_primary, e.diag.message_hint)
        finally:
            c.execute("drop table if exists e, s")
    return (None, None, None)


@pytest.mark.backlog("RP-408")
@pytest.mark.parametrize("query", list(SCENARIOS))
def test_missing_column_hint(query: str, rust_pg: object) -> None:
    assert answer(_dsn(rust_pg) + " dbname=postgres", query) == SCENARIOS[query]
