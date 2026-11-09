"""Gap tests of divergences found while working RP-694 (`SELECT ... INTO` a
scalar variable stored the column's value uncoerced):

- an assignment whose types have only an EXPLICIT cast between them (`int`
  from `bool`, `bool` from `int`) uses that cast, where PostgreSQL's
  PL/pgSQL coercion (`COERCION_PLPGSQL`) converts through the types' text
  I/O and refuses `t` as an integer (RP-705);
- a `FOR` over a query, or an `EXECUTE ... INTO`, whose target is a scalar
  variable or a list of them is refused 0A000 (RP-706);
- a variable of a NOT NULL domain with no default is created NULL, where
  PostgreSQL refuses the NULL at the block's entry (RP-707).

The expected answers were measured on a private, freshly initdb'd
PostgreSQL 15.19 cluster with `_SETUP`.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "drop domain if exists dnn694 cascade",
    "create domain dnn694 as int not null",
]


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        c.execute(_SETUP[0])


def _answer(dsn: str, query: str) -> object:
    notes: list[str] = []
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP:
            c.execute(q)
        c.add_notice_handler(lambda d: notes.append(d.message_primary))
        try:
            c.execute(query)
            return ("ok", notes)
        except psycopg.Error as e:
            return ("error", e.sqlstate, str(e).splitlines()[0])


_NOT_INT = ("error", "22P02", 'invalid input syntax for type integer: "t"')
_NOT_BOOL = ("error", "22P02", 'invalid input syntax for type boolean: "5"')

# `exec_cast_value` -> `get_cast_hashentry` coerces with COERCION_PLPGSQL:
# with no implicit or assignment cast it converts through the source type's
# output and the target's input, so `true` reaches `int4in` as `t`.
EXPLICIT_ONLY = {
    "do $$ declare x int; begin x := true; end $$": _NOT_INT,
    "do $$ declare x int := true; begin end $$": _NOT_INT,
    "do $$ declare x bool; begin x := 5; end $$": _NOT_BOOL,
    "do $$ declare x int; begin select true into x; end $$": _NOT_INT,
    "do $$ declare x bool; begin select 5 into x; end $$": _NOT_BOOL,
    # bool -> text is an assignment cast (`booltext`): it is used.
    "do $$ declare x text; begin x := true; raise notice '%', x; end $$": ("ok", ["true"]),
}


@pytest.mark.backlog("RP-705")
@pytest.mark.parametrize("query", list(EXPLICIT_ONLY))
def test_assignment_with_only_an_explicit_cast_converts_through_text(query: str, dsn: str) -> None:
    assert _answer(dsn, query) == EXPLICIT_ONLY[query]


_VARCHAR3 = ("error", "22001", "value too long for type character varying(3)")

# The grammar makes a list of scalar variables a row target
# (`read_into_scalar_list`), which `exec_move_row` fills column by column.
SCALAR_TARGETS = {
    "do $$ declare x int; begin for x in select '7'::text loop"
    " raise notice '%', x + 1; end loop; end $$": ("ok", ["8"]),
    "do $$ declare x int; y text; begin for x, y in select 1, 'a' loop"
    " raise notice '% %', x, y; end loop; end $$": ("ok", ["1 a"]),
    "do $$ declare x varchar(3); begin for x in select 'abcdef' loop end loop; end $$": (_VARCHAR3),
    "do $$ declare x int; begin execute 'select 7' into x; raise notice '%', x; end $$": (
        "ok",
        ["7"],
    ),
    "do $$ declare x int; y text; begin execute 'select 1, ''a''' into x, y;"
    " raise notice '% %', x, y; end $$": ("ok", ["1 a"]),
    "do $$ declare x varchar(3); begin execute 'select ''abcdef''' into x; end $$": _VARCHAR3,
}


@pytest.mark.backlog("RP-706")
@pytest.mark.parametrize("query", list(SCALAR_TARGETS))
def test_for_and_execute_into_scalar_variables(query: str, dsn: str) -> None:
    assert _answer(dsn, query) == SCALAR_TARGETS[query]


_NOT_NULL = ("error", "23502", "domain dnn694 does not allow null values")

# `exec_stmt_block` initialises a variable with no DEFAULT by assigning it
# NULL through `exec_assign_value`, which checks the domain's NOT NULL.
NOT_NULL_DOMAIN = {
    "do $$ declare x dnn694; begin end $$": _NOT_NULL,
    "do $$ declare x dnn694; begin select 5 into x; end $$": _NOT_NULL,
    "do $$ declare x dnn694 := 5; begin raise notice '%', x; end $$": ("ok", ["5"]),
}


@pytest.mark.backlog("RP-707")
@pytest.mark.parametrize("query", list(NOT_NULL_DOMAIN))
def test_a_not_null_domain_variable_without_a_default_is_refused(query: str, dsn: str) -> None:
    assert _answer(dsn, query) == NOT_NULL_DOMAIN[query]
