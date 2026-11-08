"""Gap tests of the divergences found while working RP-508 (DROP and ALTER
of a routine named without arguments look among the statement's kind
only): RP-514 (GRANT and COMMENT ON a routine named without arguments
looked among every kind, fixed) and RP-515 (`pg_get_function_arguments` and
`pg_get_function_identity_arguments` of a procedure print no argument
modes).

An answer is `("ok", notices)`, or `(sqlstate, message, detail, hint,
position, notices)` for an error. The expected answers were measured on a
private, freshly initdb'd PostgreSQL 15.19 cluster after `_SETUP`.
"""

from __future__ import annotations

from collections.abc import Iterator
from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_ROUTINES = [
    "gq(int)",
    "gq2(int)",
    "gm(int)",
    "gm(text)",
    "gk(int, text)",
    "go(int)",
    "go(text)",
]

_SETUP = [f"drop routine if exists {r}" for r in _ROUTINES] + [
    "create procedure gq(int) language sql as $$ select 1 $$",
    "create function gq2(int) returns int language sql as $$ select 1 $$",
    # A function and a procedure of one name.
    "create function gm(int) returns int language sql as $$ select 1 $$",
    "create procedure gm(text) language sql as $$ select 1 $$",
    "create procedure gk(a int, inout b text) language sql as $$ select 'x' $$",
    # Two functions of one name.
    "create function go(int) returns int language sql as $$ select 1 $$",
    "create function go(text) returns int language sql as $$ select 1 $$",
]


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        for r in _ROUTINES:
            c.execute(f"drop routine if exists {r}")


def answer(dsn: str, query: str) -> Any:
    """Run `query` after `_SETUP` in a transaction rolled back after it, so
    a GRANT or COMMENT that succeeds leaves nothing behind."""
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP:
            c.execute(q)
        notices: list[str | None] = []
        c.add_notice_handler(lambda d: notices.append(d.message_primary))
        c.execute("begin")
        try:
            cur = c.execute(query)
            return ("ok", cur.fetchall() if cur.description else None, notices)
        except psycopg.Error as e:
            d = e.diag
            return (
                e.sqlstate,
                d.message_primary,
                d.message_detail,
                d.message_hint,
                d.statement_position,
                notices,
            )
        finally:
            c.execute("rollback")


def _missing(kind: str, name: str) -> tuple[Any, ...]:
    return ("42883", f'could not find a {kind} named "{name}"', None, None, None, [])


# RP-514: GRANT and COMMENT ON resolve their routine through
# `LookupFuncWithArgs` too, so with no argument list the other kind is
# missing (42883), a function and a procedure of one name are not
# ambiguous to FUNCTION or PROCEDURE, and ROUTINE's 42725 says "routine".
# Before the fix both looked among every kind.
LOOKUP_BY_KIND = {
    "grant execute on function gq to public": _missing("function", "gq"),
    "grant execute on procedure gq2 to public": _missing("procedure", "gq2"),
    "grant execute on function gm to public": ("ok", None, []),
    "grant execute on routine gm to public": (
        "42725",
        'routine name "gm" is not unique',
        None,
        "Specify the argument list to select the routine unambiguously.",
        None,
        [],
    ),
    "comment on function gq is 'x'": _missing("function", "gq"),
    "comment on procedure gq2 is 'x'": _missing("procedure", "gq2"),
    "comment on function gm is 'x'": ("ok", None, []),
}


@pytest.mark.backlog_fixed("RP-514")
@pytest.mark.parametrize("query", list(LOOKUP_BY_KIND))
def test_grant_and_comment_look_up_by_kind(query: str, dsn: str) -> None:
    assert answer(dsn, query) == LOOKUP_BY_KIND[query]


def _error(state: str, message: str, hint: str | None = None) -> tuple[Any, ...]:
    return (state, message, None, hint, None, [])


# RP-514's regression: COMMENT ON a routine resolved it by its last name
# alone, dropping the argument list and the schema, and refused ROUTINE as
# 0A000. It now names the routine by its signature, as GRANT does, so an
# overload is found by its types, a signature of the other kind is 42809,
# and a missing signature is 42883 in the statement's own noun.
COMMENT_BY_SIGNATURE = {
    "comment on function gq(int) is 'x'": _error("42809", "gq(integer) is not a function"),
    "comment on procedure gq2(int) is 'x'": _error("42809", "gq2(integer) is not a procedure"),
    "comment on function gm(int) is 'x'": ("ok", None, []),
    "comment on procedure gm is 'x'": ("ok", None, []),
    "comment on routine gm is 'x'": _error(
        "42725",
        'routine name "gm" is not unique',
        "Specify the argument list to select the routine unambiguously.",
    ),
    "comment on routine gq is 'x'": ("ok", None, []),
    "comment on routine gq(int) is 'x'": ("ok", None, []),
    "comment on function go is 'x'": _error(
        "42725",
        'function name "go" is not unique',
        "Specify the argument list to select the function unambiguously.",
    ),
    "comment on function go(text) is 'x'": ("ok", None, []),
    "comment on function go(bool) is 'x'": _error("42883", "function go(boolean) does not exist"),
    "comment on procedure gq(text) is 'x'": _error("42883", "procedure gq(text) does not exist"),
    "comment on function nosuch is 'x'": _missing("function", "nosuch"),
    "comment on procedure nosuch is 'x'": _missing("procedure", "nosuch"),
    "comment on routine nosuch is 'x'": _missing("function", "nosuch"),
    "comment on function nos.gq is 'x'": _error("3F000", 'schema "nos" does not exist'),
    "grant execute on routine gq to public": ("ok", None, []),
    "grant execute on function go to public": _error(
        "42725",
        'function name "go" is not unique',
        "Specify the argument list to select the function unambiguously.",
    ),
    "grant execute on procedure nosuch to public": _missing("procedure", "nosuch"),
    "grant execute on routine nosuch to public": _missing("function", "nosuch"),
}


@pytest.mark.backlog_fixed("RP-514")
@pytest.mark.parametrize("query", list(COMMENT_BY_SIGNATURE))
def test_comment_names_a_routine_by_its_signature(query: str, dsn: str) -> None:
    assert answer(dsn, query) == COMMENT_BY_SIGNATURE[query]


@pytest.mark.backlog_fixed("RP-514")
def test_comment_lands_on_the_named_overload(dsn: str) -> None:
    """COMMENT ON FUNCTION go(text) described go(text) and not go(int)
    (42725 before the fix); read back after it commits."""
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP:
            c.execute(q)
        c.execute("comment on function go(text) is 'y'")
        c.execute("comment on routine gq is 'z'")
        got = c.execute(
            "select obj_description('go(text)'::regprocedure, 'pg_proc'), "
            "obj_description('go(int)'::regprocedure, 'pg_proc'), "
            "obj_description('gq(int)'::regprocedure, 'pg_proc')"
        ).fetchall()
    assert got == [("y", None, "z")]


# RP-515: `print_function_arguments` prints a procedure's every argument
# with its mode, IN included (a function's IN is left out).
@pytest.mark.backlog("RP-515")
def test_procedure_arguments_print_their_modes(dsn: str) -> None:
    query = (
        "select pg_get_function_identity_arguments('gk'::regproc), "
        "pg_get_function_arguments('gk'::regproc)"
    )
    assert answer(dsn, query) == (
        "ok",
        [("IN a integer, INOUT b text", "IN a integer, INOUT b text")],
        [],
    )
