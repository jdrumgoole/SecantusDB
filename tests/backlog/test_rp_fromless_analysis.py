"""Parse-analysis errors of statements with no FROM, found while working
RP-439: a missing column inside an expression was refused as a gap (RP-441,
fixed), and a cast literal's input error is reported after the operator's
(RP-442, open).

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


@pytest.mark.backlog_fixed("RP-441")
@pytest.mark.parametrize("query", list(MISSING_IN_EXPRESSION))
def test_missing_column_in_an_expression_with_no_from(query: str, rust_pg: Any) -> None:
    """A bare `select nosuch` was 42703 on both; the same name inside an
    operator, a function or a CASE was 0A000 `ColumnRef is not supported
    yet` here (`abs(nosuch)` 42883 `function abs(text)`)."""
    assert answer(rust_pg, query) == MISSING_IN_EXPRESSION[query]


def _no_function(position: str) -> Answer:
    return (
        "42883",
        "function nosuchfn(integer) does not exist",
        "No function matches the given name and argument types. "
        "You might need to add explicit type casts.",
        position,
    )


# The same lookup in every clause `transformSelectStmt` analyses, and for a
# qualified name (`errorMissingRTE`, 42P01). A bare name heading an ORDER
# BY or GROUP BY item may name an output column instead, and what is left
# of the reference is analysed first: an untyped literal under a cast is
# read by its type's input function, and a call is looked up. Measured on
# PostgreSQL 15.19 while working RP-441.
REFERENCE_IN_EVERY_CLAUSE: dict[str, Answer] = {
    "select nosuch.a + 1": ("42P01", 'missing FROM-clause entry for table "nosuch"', None, "8"),
    "select 1 + x.y.z": ("42P01", 'missing FROM-clause entry for table "y"', None, "12"),
    "select nosuch.*": ("42P01", 'missing FROM-clause entry for table "nosuch"', None, "8"),
    "select postgres.public.t.a": ("42P01", 'missing FROM-clause entry for table "t"', None, "8"),
    "select other.public.t.a": (
        "0A000",
        "cross-database references are not implemented: other.public.t.a",
        None,
        "8",
    ),
    "select a.b.c.d.e": (
        "42601",
        "improper qualified name (too many dotted names): a.b.c.d.e",
        None,
        "8",
    ),
    "select current_database": ("42703", 'column "current_database" does not exist', None, "8"),
    "select version": ("42703", 'column "version" does not exist', None, "8"),
    'select "NoSuch" + 1': ("42703", 'column "NoSuch" does not exist', None, "8"),
    "select null + nosuch": _missing("15"),
    "select 1/0, nosuch": _missing("13"),
    "select 1 where 1 + nosuch = 2": _missing("20"),
    "select 1 as x order by x + 1": ("42703", 'column "x" does not exist', None, "24"),
    "select 1 as x order by nosuch": _missing("24"),
    "select 1 group by nosuch": _missing("19"),
    "select distinct on (nosuch) 1": _missing("21"),
    "select 1 offset nosuch limit nosuch2": _missing("17"),
    "select 1 limit nosuch2 offset nosuch": _missing("31"),
    "select 1 x window w as (order by nosuch)": _missing("34"),
    "select 'x'::int, 1 + nosuch": (
        "22P02",
        'invalid input syntax for type integer: "x"',
        None,
        "8",
    ),
    "select int 'x' + nosuch": ("22P02", 'invalid input syntax for type integer: "x"', None, "12"),
    "select ('x' || 'y')::int + nosuch": _missing("28"),
    "select nosuchfn(1), nosuch": _no_function("8"),
    "select nosuchfn(1) + nosuch": _no_function("8"),
    "select abs(nosuchfn(1)), nosuch": _no_function("12"),
    "select count(1), nosuch": _missing("18"),
    "select 1 as x order by x": None,
    "select abs(1) order by abs": None,
    "select 1 x group by x order by x": None,
}


@pytest.mark.backlog_fixed("RP-441")
@pytest.mark.parametrize("query", list(REFERENCE_IN_EVERY_CLAUSE))
def test_reference_in_every_clause_with_no_from(query: str, rust_pg: Any) -> None:
    """A qualified name was 0A000 inside an expression and 42703 alone, and
    a missing name in ORDER BY, GROUP BY or DISTINCT ON was ignored."""
    assert answer(rust_pg, query) == REFERENCE_IN_EVERY_CLAUSE[query]


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


# Found while working RP-441, each measured on PostgreSQL 15.19.

EXISTS_TARGETS = {
    "select exists (select nosuch)": _missing("23"),
    "select 1 where exists (select nosuch)": _missing("31"),
    "select exists (select nosuch from (select 1) s)": _missing("23"),
}


@pytest.mark.backlog("RP-444")
@pytest.mark.parametrize("query", list(EXISTS_TARGETS))
def test_exists_target_list_is_analysed(query: str, rust_pg: Any) -> None:
    """The subquery's targets are dropped unanalysed, so the statement
    runs (`true`, or a row) where PostgreSQL refuses it."""
    assert answer(rust_pg, query) == EXISTS_TARGETS[query]


@pytest.mark.backlog("RP-445")
@pytest.mark.parametrize(
    "query",
    [
        "select (select (select a)) from (select 1 a) t",
        "select (select (select t.a)) from (select 1 a) t",
    ],
)
def test_fromless_subquery_two_levels_down_reads_the_outer_row(query: str, rust_pg: Any) -> None:
    """42703 with a cannot-be-referenced HINT, and a leaked internal
    `\\x1fcorrelated` call, where PostgreSQL answers the outer row's 1."""
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        try:
            got: object = c.execute(query).fetchall()
        except psycopg.Error as e:
            got = (e.sqlstate, e.diag.message_primary)
    assert got == [(1,)]


@pytest.mark.backlog("RP-446")
def test_star_with_no_from(rust_pg: Any) -> None:
    """`0A000 this target is not supported yet` here."""
    assert answer(rust_pg, "select *") == (
        "42601",
        "SELECT * with no tables specified is not valid",
        None,
        "8",
    )


@pytest.mark.backlog("RP-447")
def test_missing_column_before_operator_to_its_right(rust_pg: Any) -> None:
    """42883 `operator does not exist: integer + boolean` at 20 here."""
    assert answer(rust_pg, "select nosuch + (1 + true)") == _missing("8")


def _no_operator(message: str, position: str) -> Answer:
    return (
        "42883",
        message,
        "No operator matches the given name and argument types. "
        "You might need to add explicit type casts.",
        position,
    )


KEYWORD_OPERANDS = {
    "select 1 + current_user": _no_operator("operator does not exist: integer + name", "10"),
    "select current_schema + 1": _no_operator("operator does not exist: name + integer", "23"),
}


@pytest.mark.backlog("RP-448")
@pytest.mark.parametrize("query", list(KEYWORD_OPERANDS))
def test_keyword_function_operand_is_name(query: str, rust_pg: Any) -> None:
    """The operand is named `text` in the message here."""
    assert answer(rust_pg, query) == KEYWORD_OPERANDS[query]
