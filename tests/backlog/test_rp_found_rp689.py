"""A PL/pgSQL variable of a composite type (RP-689), and the divergences
found while working it:

- a variable declared of a table's row type (`declare r t`), a composite
  type's or a relation's `%ROWTYPE` was a scalar, so `r.a := 5` did not
  parse and a row moved into it kept only its first column (RP-689,
  fixed: these are its regression tests beside the gap test in
  `test_rp_found_rp670.py`);
- reading a field a record does not have was 42P01 (RP-837, fixed);
- a qualified column reference that a record variable's field shares is
  read as the field, not refused as ambiguous (RP-838).

The expected answers were measured on a private, freshly initdb'd
PostgreSQL 15.19 cluster with `_SETUP`.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "drop table if exists t689 cascade",
    "drop type if exists ct689 cascade",
    "drop domain if exists d689, dv689 cascade",
    "drop schema if exists s689 cascade",
    "create domain d689 as int check (value > 0)",
    "create domain dv689 as varchar(3)",
    "create table t689 (a d689, v varchar(3), w dv689, c char(3))",
    "insert into t689 values (5, 'ab', 'q', 'x')",
    "create type ct689 as (x int, y text)",
    "create schema s689",
    "create table s689.st (k int, l text)",
]
_R = "do $$ declare r t689; begin %s end $$"


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        for q in _SETUP:
            c.execute(q)
    yield _dsn(rust_pg) + " dbname=postgres"


def _answer(dsn: str, query: str) -> object:
    with psycopg.connect(dsn, autocommit=True) as c:
        try:
            cur = c.execute(query)
            return cur.fetchall() if cur.description else cur.statusmessage
        except psycopg.Error as e:
            return ("error", e.sqlstate, str(e).splitlines()[0])


def _raised(text: str) -> tuple[str, str, str]:
    return ("error", "P0001", text)


# A record of a row type is NULL until assigned, each field NULL of the
# field's type; assigning a field first makes it a row of NULL fields
# (`instantiate_empty_record_variable`). A row moved into it is converted
# to its fields by position, by the assignment rule (`exec_move_row`).
ROW_TYPED = {
    _R % "raise exception '%', pg_typeof(r);": _raised("t689"),
    _R % "raise exception '[%]', r;": _raised("[<NULL>]"),
    _R % "raise exception '%', r is null;": _raised("t"),
    _R % "raise exception '%', r.a is null;": _raised("t"),
    _R % "raise exception '%', pg_typeof(r.v);": _raised("character varying"),
    _R % "r.nosuch := 1;": ("error", "42703", 'record "r" has no field "nosuch"'),
    _R % "r.v := 'x'; raise exception '[%] %', r, r is null;": _raised("[(,x,,)] f"),
    _R % "select * into r from t689 where false; raise exception '[%] %', r, r is null;": (
        _raised("[(,,,)] t")
    ),
    _R % "select a, v into r from t689; raise exception '[%]', r;": _raised("[(5,ab,,)]"),
    _R % "select 7, 'zz', 'w', 'c', 99 into r; raise exception '[%]', r;": (
        _raised('[(7,zz,w,"c  ")]')
    ),
    _R % "select '-3' into r; raise exception '[%]', r;": (
        "error",
        "23514",
        'value for domain d689 violates check constraint "d689_check"',
    ),
    _R % "for r in select * from t689 loop raise exception '[%]', r; end loop;": (
        _raised('[(5,ab,q,"x  ")]')
    ),
    _R % "r.a := 2; r.a := r.a + 1; raise exception '[%]', r.a;": _raised("[3]"),
    _R % "r.w := 'abcd';": ("error", "22001", "value too long for type character varying(3)"),
    "do $$ declare r t689%rowtype; begin r.c := 'y'; raise exception '[%]', r; end $$": (
        _raised('[(,,,"y  ")]')
    ),
    "do $$ declare r ct689; begin r.x := 1; r.y := 'q'; "
    "raise exception '[%] %', r, pg_typeof(r); end $$": _raised("[(1,q)] ct689"),
    "do $$ declare r s689.st; begin r.k := 1; "
    "raise exception '[%] %', r, pg_typeof(r); end $$": _raised("[(1,)] s689.st"),
    "do $$ declare r public.t689; begin r.a := 2; raise exception '[%]', r; end $$": (
        _raised("[(2,,,)]")
    ),
    "do $$ declare r constant t689 := null; begin raise exception '[%]', r; end $$": (
        _raised("[<NULL>]")
    ),
}


@pytest.mark.backlog_fixed("RP-689")
@pytest.mark.parametrize("query", list(ROW_TYPED))
def test_a_variable_of_a_row_type_is_a_record_of_it(query: str, dsn: str) -> None:
    """Was: the variable was a scalar of the type's name (RP-689)."""
    assert _answer(dsn, query) == ROW_TYPED[query]


@pytest.mark.backlog_fixed("RP-689")
def test_a_function_compiles_a_row_typed_variable(dsn: str) -> None:
    """Was: CREATE FUNCTION refused the body, 42601 `"r.w" is not a known
    variable` (RP-689)."""
    create = (
        "create or replace function f689() returns text language plpgsql as "
        "$$ declare r t689; begin r.w := 'abc'; return r::text; end $$"
    )
    assert _answer(dsn, create) == "CREATE FUNCTION"
    assert _answer(dsn, "select f689()") == [("(,,abc,)",)]


# `exec_eval_datum` over a RECFIELD looks the field up in the record's row
# type, and a name it lacks is 42703, whatever filled the record.
MISSING_FIELD = {
    "do $$ declare r record; begin select 1 as a into r; raise exception '%', r.nosuch; end $$": (
        "error",
        "42703",
        'record "r" has no field "nosuch"',
    ),
    _R % "raise exception '%', r.nosuch;": ("error", "42703", 'record "r" has no field "nosuch"'),
}
_NOSUCH = ("error", "42703", 'record "r" has no field "nosuch"')
_FILLED = "do $$ declare r record; x int; begin select 1 as a into r; %s end $$"
# The same read in every statement that evaluates an expression, and a
# record that hides a block labelled with its name: `plpgsql_ns_lookup`
# searches each level for the unqualified name before the level's label.
MISSING_FIELD_EVERYWHERE = {
    _FILLED % "x := r.nosuch;": _NOSUCH,
    _FILLED % "if r.nosuch then end if;": _NOSUCH,
    _FILLED % "perform r.nosuch;": _NOSUCH,
    _FILLED % "select r.nosuch into x;": _NOSUCH,
    _FILLED % "raise exception '%', (select r.nosuch from (select 1) r);": _NOSUCH,
    "do $$ <<r>> declare nosuch int := 7; begin declare r record; begin "
    "select 1 as a into r; raise exception '%', r.nosuch; end; end $$": _NOSUCH,
    # The field it has is still read, and a label still qualifies when no
    # record hides it.
    _FILLED % "raise exception '%', r.a;": _raised("1"),
    "do $$ <<r>> declare nosuch int := 7; begin raise exception '%', r.nosuch; end $$": (
        _raised("7")
    ),
}


@pytest.mark.backlog_fixed("RP-837")
@pytest.mark.parametrize("query", list(MISSING_FIELD))
def test_reading_a_field_a_record_lacks_is_42703(query: str, dsn: str) -> None:
    """Was: the reference was left in the SQL text, which read it as a
    column of a FROM item `r`: 42P01 `missing FROM-clause entry for table
    "r"` (RP-837)."""
    assert _answer(dsn, query) == MISSING_FIELD[query]


@pytest.mark.backlog_fixed("RP-837")
@pytest.mark.parametrize("query", list(MISSING_FIELD_EVERYWHERE))
def test_a_missing_record_field_is_42703_in_every_statement(query: str, dsn: str) -> None:
    """Was: 42P01 in each, `7` for the hidden label (RP-837)."""
    assert _answer(dsn, query) == MISSING_FIELD_EVERYWHERE[query]


@pytest.mark.backlog("RP-838")
def test_a_column_a_record_field_shares_is_ambiguous(dsn: str) -> None:
    # `plpgsql_variable_conflict` is `error` by default: `r.a` names both
    # the FROM item `r`'s column and the variable `r`'s field.
    query = _R % "raise exception '%', (select r.a from t689 r);"
    assert _answer(dsn, query) == ("error", "42702", 'column reference "r.a" is ambiguous')
