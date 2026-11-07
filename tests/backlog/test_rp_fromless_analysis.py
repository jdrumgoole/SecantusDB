"""Gap tests for parse-analysis errors of statements with no FROM, found
while working RP-439: a missing column inside an expression is refused as a
gap, and a cast literal's input error is reported after the operator's.

An answer is `(sqlstate, message, hint, position)`, or `None` for a
statement that succeeds. Each was measured on PostgreSQL 15.19; none needs
a table, so neither side's answer depends on a schema.
"""

from __future__ import annotations

from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

Answer = tuple[str | None, str | None, str | None, str | None] | None


def answer(server: Any, query: str) -> Answer:
    with psycopg.connect(_dsn(server) + " dbname=postgres", autocommit=True) as c:
        try:
            c.execute(query)
        except psycopg.Error as e:
            return (
                e.sqlstate,
                e.diag.message_primary,
                e.diag.message_hint,
                e.diag.statement_position,
            )
    return None


def _missing(position: str) -> Answer:
    return ("42703", 'column "nosuch" does not exist', None, position)


# `transformColumnRef` with an empty range table: 42703 at the name, with
# no HINT (there is no column to suggest), wherever the name sits.
MISSING_IN_EXPRESSION = {
    "select 1 + nosuch": _missing("12"),
    "select -nosuch": _missing("9"),
    "select coalesce(nosuch, 1)": _missing("17"),
    "select case when nosuch then 1 end": _missing("18"),
    "select case 1 when 1 then nosuch end": _missing("27"),
    "select case nosuch when 1 then 1 end": _missing("13"),
    "select abs(nosuch)": _missing("12"),
}


@pytest.mark.backlog("RP-441")
@pytest.mark.parametrize("query", list(MISSING_IN_EXPRESSION))
def test_missing_column_in_an_expression_with_no_from(query: str, rust_pg: Any) -> None:
    """A bare `select nosuch` is 42703 on both; the same name inside an
    operator, a function or a CASE is 0A000 `ColumnRef is not supported
    yet` here (`abs(nosuch)` 42883 `function abs(text)`)."""
    assert answer(rust_pg, query) == MISSING_IN_EXPRESSION[query]


# `transformTypeCast` over a constant runs the type's input function as the
# cast is transformed, so its error comes before the operator around it is
# looked up (`make_op` runs after both operands are transformed).
CAST_BEFORE_OPERATOR = {
    "select 'x'::int = true": (
        "22P02",
        'invalid input syntax for type integer: "x"',
        None,
        "8",
    ),
    "select 'x'::int + 'y'::bool": (
        "22P02",
        'invalid input syntax for type integer: "x"',
        None,
        "8",
    ),
    "select 1 = 'x'::uuid": ("22P02", 'invalid input syntax for type uuid: "x"', None, "12"),
    "select case 'x'::uuid when 1 then 1 end": (
        "22P02",
        'invalid input syntax for type uuid: "x"',
        None,
        "13",
    ),
    "select '2020-13-01'::date = 1": (
        "22008",
        'date/time field value out of range: "2020-13-01"',
        'Perhaps you need a different "datestyle" setting.',
        "8",
    ),
}


@pytest.mark.backlog("RP-442")
@pytest.mark.parametrize("query", list(CAST_BEFORE_OPERATOR))
def test_cast_literal_input_error_before_operator(query: str, rust_pg: Any) -> None:
    """A lone `'x'::int` is 22P02 on both; beside an operator with no
    overload for the two types it is that operator's 42883 here."""
    assert answer(rust_pg, query) == CAST_BEFORE_OPERATOR[query]
