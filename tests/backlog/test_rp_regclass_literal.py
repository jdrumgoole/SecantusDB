"""Regression tests of a string literal read as a `regclass` while the
statement is analysed (RP-458, found while working RP-452).

PostgreSQL coerces an untyped literal argument to its parameter's type as
the call is analysed (`coerce_type` runs `regclassin`), so a relation the
literal names that does not exist is 42P01 before the statement is planned
or run, wherever the call sits. An explicit cast of a literal to
`regclass` is coerced the same way, and the error comes in the order the
transform meets it: after a missing FROM relation or an earlier missing
column, ahead of an operator over it. An answer is `(sqlstate, message,
hint, position)`, or the rows of a statement that succeeds. Each was
measured on PostgreSQL 15.19; none needs a table.
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


@pytest.mark.backlog_fixed("RP-458")
@pytest.mark.parametrize("query", list(MISSING_AT_ANALYSIS))
def test_regclass_literal_is_resolved_at_analysis(query: str, rust_pg: Any) -> None:
    """The relation was looked up only when the call ran, if it ran."""
    assert answer(rust_pg, query) == MISSING_AT_ANALYSIS[query]


def _missing_column(position: str) -> tuple[str, str, None, str]:
    return ("42703", 'column "nosuch" does not exist', None, position)


IN_ANALYSIS_ORDER = {
    # A cast of a literal, in each of its spellings, and the call form.
    "select 1 where false and 'nosuchseq'::regclass::oid > 0": _missing("26"),
    "select 1 where false and cast('nosuchseq' as regclass)::oid > 0": _missing("31"),
    "select 1 where false and regclass 'nosuchseq' is null": _missing("35"),
    "select 1 where false and regclass('nosuchseq') is null": _missing("35"),
    # Other built-ins' `regclass` parameters, one overload of several too.
    "select 1 where false and pg_relation_size('nosuchseq') > 0": _missing("43"),
    "select 1 where false and pg_relation_size('nosuchseq', 'main') > 0": _missing("43"),
    "select 1 where false and pg_table_size('nosuchseq') > 0": _missing("40"),
    "select 1 where false and pg_catalog.nextval('nosuchseq') > 0": _missing("45"),
    # Wherever the call sits.
    "select case when false then nextval('nosuchseq') end": _missing("37"),
    "select coalesce(1, nextval('nosuchseq'))": _missing("28"),
    "with w as (select nextval('nosuchseq')) select 1": _missing("27"),
    "select 1 union select nextval('nosuchseq') where false": _missing("31"),
    "select 1 order by nextval('nosuchseq')": _missing("27"),
    "select 1 where exists (select nextval('nosuchseq') where false)": _missing("39"),
    "select 1 from pg_class where false and nextval('nosuchseq') > 0": _missing("48"),
    "explain select 1 where false and nextval('nosuchseq') > 0": _missing("42"),
    # The operands before the operator, left before right.
    "select nextval('nosuchseq') + true": _missing("16"),
    "select nextval('nosuchseq'), nosuch": _missing("16"),
    "select nextval('nosuchseq') where nosuch": _missing("16"),
    # An earlier error is still the first one.
    "select nosuch + nextval('nosuchseq')": _missing_column("8"),
    "select nosuch, nextval('nosuchseq')": _missing_column("8"),
    "select nextval('nosuchseq') from nosuchtable": (
        "42P01",
        'relation "nosuchtable" does not exist',
        None,
        "34",
    ),
    # A literal naming a relation that is there passes (no rows).
    "select 1 where false and pg_relation_size('pg_class') > 0": [],
}


@pytest.mark.backlog_fixed("RP-458")
@pytest.mark.parametrize("query", list(IN_ANALYSIS_ORDER))
def test_regclass_literal_in_analysis_order(query: str, rust_pg: Any) -> None:
    """A cast, another built-in, any clause, and the transform's order."""
    assert answer(rust_pg, query) == IN_ANALYSIS_ORDER[query]
