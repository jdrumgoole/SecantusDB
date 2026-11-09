"""Gap tests of divergences found while working RP-670 (a PL/pgSQL
assignment to a trigger record's domain field did not check the domain):

- a PL/pgSQL assignment to a `varchar(n)` / `char(n)` variable, or one of a
  domain over `varchar(n)`, truncates an overlong value where PostgreSQL's
  assignment coercion raises 22001 (RP-687);
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
}


@pytest.mark.backlog("RP-687")
@pytest.mark.parametrize("query", list(VARIABLE_LENGTH))
def test_variable_assignment_refuses_an_overlong_value(query: str, dsn: str) -> None:
    assert _answer(dsn, query) == VARIABLE_LENGTH[query]


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
