"""Gap tests of divergences found while working RP-670 (a PL/pgSQL
assignment to a trigger record's domain field did not check the domain):

- a PL/pgSQL assignment to a `varchar(n)` / `char(n)` variable, or one of a
  domain over `varchar(n)`, truncated an overlong value where PostgreSQL's
  assignment coercion raises 22001 (RP-687, fixed: now regression tests);
- a record filled by `SELECT ... INTO` types its fields by the wire's base
  type, without the column's domain or modifier, so an assignment to a
  field checks neither (RP-688);
- a variable declared of a table's row type (`declare r t`) has no fields
  an assignment can name (RP-689).

The expected answers were measured on a private, freshly initdb'd
PostgreSQL 15.19 cluster with `_SETUP`.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "drop table if exists t687 cascade",
    "drop domain if exists d687, dv687 cascade",
    "create domain d687 as int check (value > 0)",
    "create domain dv687 as varchar(3)",
    "create table t687 (a d687, v varchar(3), w dv687, c char(3))",
    "insert into t687 values (5, 'ab', 'q', 'x')",
]
_SEL = "do $$ declare r record; begin select * into r from t687; %s end $$"
_ROW = "do $$ declare r t687; begin %s end $$"
_VARCHAR3 = ("error", "22001", "value too long for type character varying(3)")
_D687 = ("error", "23514", 'value for domain d687 violates check constraint "d687_check"')


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        for q in _SETUP[:2]:
            c.execute(q)


def _answer(dsn: str, query: str) -> object:
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP:
            c.execute(q)
        try:
            return c.execute(query).statusmessage
        except psycopg.Error as e:
            return ("error", e.sqlstate, str(e).splitlines()[0])


# `exec_assign_value` coerces with the assignment rule (`exec_cast_value`,
# COERCION_PLPGSQL), whose length coercion refuses an overlong value; an
# explicit CAST would truncate it.
VARIABLE_LENGTH = {
    "do $$ declare x varchar(3); begin x := 'abcdef'; end $$": _VARCHAR3,
    "do $$ declare x varchar(3); begin x := 'abcdef'::varchar(5); end $$": _VARCHAR3,
    "do $$ declare x dv687; begin x := 'abcdef'; end $$": _VARCHAR3,
    "do $$ declare x char(3); begin x := 'abcd'; end $$": (
        "error",
        "22001",
        "value too long for type character(3)",
    ),
    # A DEFAULT is assigned by the same rule (`exec_stmt_block`).
    "do $$ declare x varchar(3) := 'abcdef'; begin end $$": _VARCHAR3,
    "do $$ declare x dv687 := 'abcdef'; begin end $$": _VARCHAR3,
    # A bare `char` is `char(1)`.
    "do $$ declare x char; begin x := 'ab'; end $$": (
        "error",
        "22001",
        "value too long for type character(1)",
    ),
    "do $$ declare x character varying(2); begin x := 12345; end $$": (
        "error",
        "22001",
        "value too long for type character varying(2)",
    ),
    "do $$ declare x varchar(3); y text := 'abcdef'; begin x := y; end $$": _VARCHAR3,
    # A `bit(n)` of another length is 22026 either way; a `varbit(n)` too
    # long is 22001.
    "do $$ declare x bit(3); begin x := B'10101'; end $$": (
        "error",
        "22026",
        "bit string length 5 does not match type bit(3)",
    ),
    "do $$ declare x bit(3); begin x := B'1'; end $$": (
        "error",
        "22026",
        "bit string length 1 does not match type bit(3)",
    ),
    "do $$ declare x varbit(3); begin x := B'10101'; end $$": (
        "error",
        "22001",
        "bit string too long for type bit varying(3)",
    ),
}


@pytest.mark.backlog_fixed("RP-687")
@pytest.mark.parametrize("query", list(VARIABLE_LENGTH))
def test_variable_assignment_refuses_an_overlong_value(query: str, dsn: str) -> None:
    """Was: each value was cast explicitly and stored truncated (RP-687)."""
    assert _answer(dsn, query) == VARIABLE_LENGTH[query]


# What fits is stored as the declared type: excess blanks dropped, a
# `char(n)` padded, a domain's CHECK and a NOT NULL still checked.
VARIABLE_FITS = {
    "do $$ declare x char(3); begin x := 'ab   '; raise exception '[%]', x; end $$": (
        "error",
        "P0001",
        "[ab ]",
    ),
    "do $$ declare x char(3); begin x := 'a'; "
    "raise exception '[%] %', x, octet_length(x); end $$": (
        "error",
        "P0001",
        "[a  ] 3",
    ),
    "do $$ declare x varchar(3); begin x := 'abc   '; raise exception '[%]', x; end $$": (
        "error",
        "P0001",
        "[abc]",
    ),
    "do $$ declare x bit(3); begin x := B'101'; raise exception '[%]', x; end $$": (
        "error",
        "P0001",
        "[101]",
    ),
    "do $$ declare x numeric(3,1); begin x := 12.345; raise exception '[%]', x; end $$": (
        "error",
        "P0001",
        "[12.3]",
    ),
    "do $$ declare x d687; begin x := -5; end $$": _D687,
}


@pytest.mark.backlog_fixed("RP-687")
@pytest.mark.parametrize("query", list(VARIABLE_FITS))
def test_variable_assignment_stores_a_fitting_value(query: str, dsn: str) -> None:
    """A value that fits is stored as the declared type (RP-687)."""
    assert _answer(dsn, query) == VARIABLE_FITS[query]


# A record's fields are the query's columns, of their declared types
# (`exprType` / `exprTypmod`): the domain, and the modifier.
SELECT_INTO_FIELDS = {
    _SEL % "r.a := -5;": _D687,
    _SEL % "r.v := 'abcdef';": _VARCHAR3,
    _SEL % "r.w := 'abcdef';": _VARCHAR3,
    _SEL % "raise exception '%', pg_typeof(r.a);": ("error", "P0001", "d687"),
    _SEL % "raise exception '%', pg_typeof(r.w);": ("error", "P0001", "dv687"),
}


@pytest.mark.backlog("RP-688")
@pytest.mark.parametrize("query", list(SELECT_INTO_FIELDS))
def test_select_into_record_fields_keep_their_declared_types(query: str, dsn: str) -> None:
    assert _answer(dsn, query) == SELECT_INTO_FIELDS[query]


# `declare r t687` is a row variable of the table's row type: each field is
# a column, assignable and of the column's type.
ROW_VARIABLE = {
    _ROW % "r.a := 5; raise exception '%', r.a;": ("error", "P0001", "5"),
    _ROW % "r.a := -5;": _D687,
    _ROW % "select * into r from t687; r.v := 'abcdef';": _VARCHAR3,
    _ROW % "select * into r from t687; raise exception '%', r;": (
        "error",
        "P0001",
        '(5,ab,q,"x  ")',
    ),
}


@pytest.mark.backlog("RP-689")
@pytest.mark.parametrize("query", list(ROW_VARIABLE))
def test_row_variable_fields_are_assignable(query: str, dsn: str) -> None:
    assert _answer(dsn, query) == ROW_VARIABLE[query]
