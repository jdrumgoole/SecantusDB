"""Gap test of a string literal given to a `regclass` parameter, found while
working RP-452.

PostgreSQL coerces an untyped literal argument to its parameter's type as
the call is analysed (`coerce_type` runs `regclassin`), so a relation the
literal names that does not exist is 42P01 before the statement is planned
or run, wherever the call sits. An answer is `(sqlstate, message, hint,
position)`, or the rows of a statement that succeeds. Each was measured on
PostgreSQL 15.19; none needs a table.
"""

from __future__ import annotations

from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn


def answer(server: Any, query: str) -> object:
    with psycopg.connect(_dsn(server) + " dbname=postgres", autocommit=True) as c:
        try:
            return c.execute(query).fetchall()
        except psycopg.Error as e:
            return (
                e.sqlstate,
                e.diag.message_primary,
                e.diag.message_hint,
                e.diag.statement_position,
            )


def _missing(position: str) -> tuple[str, str, None, str]:
    return ("42P01", 'relation "nosuchseq" does not exist', None, position)


MISSING_AT_ANALYSIS = {
    # Never evaluated, still analysed: zero rows and a NULL here.
    "select 1 where false and nextval('nosuchseq') > 0": _missing("34"),
    "select (select nextval('nosuchseq') where false)": _missing("24"),
    # Ahead of the subquery's width and of the operator to its right.
    "select (select nextval('nosuchseq'), 2)": _missing("24"),
    "select (select currval('nosuchseq'), 2)": _missing("24"),
    "select nextval('nosuchseq') + (1 + true)": _missing("16"),
}


@pytest.mark.backlog("RP-458")
@pytest.mark.parametrize("query", list(MISSING_AT_ANALYSIS))
def test_regclass_literal_is_resolved_at_analysis(query: str, rust_pg: Any) -> None:
    """The relation is looked up only when the call runs, if it runs."""
    assert answer(rust_pg, query) == MISSING_AT_ANALYSIS[query]
