"""Gap tests of the divergences found while working RP-514 (GRANT and
COMMENT ON a routine resolve it as `LookupFuncWithArgs` does).

The expected answers were measured on a private, freshly initdb'd
PostgreSQL 15.19 cluster. An answer is `("ok", rows)`, or `(sqlstate,
message, hint, position)` for an error.
"""

from __future__ import annotations

from collections.abc import Iterator
from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_ROUTINES = ["gc(int)", "gc(text)"]

_SETUP = [f"drop routine if exists {r}" for r in _ROUTINES] + [
    "create function gc(int) returns int language sql as $$ select 1 $$",
    "create function gc(text) returns int language sql as $$ select 1 $$",
]


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        for r in _ROUTINES:
            c.execute(f"drop routine if exists {r}")


def answer(dsn: str, *queries: str) -> Any:
    """Run `queries` after `_SETUP` in one transaction rolled back after
    them; the last one's rows (or error) is the answer."""
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP:
            c.execute(q)
        c.execute("begin")
        try:
            for q in queries:
                cur = c.execute(q)
            return ("ok", cur.fetchall() if cur.description else None)
        except psycopg.Error as e:
            d = e.diag
            return (e.sqlstate, d.message_primary, d.message_hint, d.statement_position)
        finally:
            c.execute("rollback")


@pytest.mark.backlog("RP-518")
def test_function_comment_seen_in_its_transaction(dsn: str) -> None:
    """A COMMENT ON FUNCTION is read back by `obj_description` in the
    transaction that made it, as a COMMENT ON TABLE already is."""
    got = answer(
        dsn,
        "comment on function gc(int) is 'w'",
        "select obj_description('gc(int)'::regprocedure, 'pg_proc')",
    )
    assert got == ("ok", [("w",)])


_AMBIGUOUS = "Specify the argument list to select the {} unambiguously."

# A built-in routine is resolved among its own overloads and kind.
BUILTIN_ROUTINES = {
    "grant execute on function lower to public": (
        "42725",
        'function name "lower" is not unique',
        _AMBIGUOUS.format("function"),
        None,
    ),
    "comment on function lower is 'x'": (
        "42725",
        'function name "lower" is not unique',
        _AMBIGUOUS.format("function"),
        None,
    ),
    "comment on routine abs is 'x'": (
        "42725",
        'routine name "abs" is not unique',
        _AMBIGUOUS.format("routine"),
        None,
    ),
    "grant execute on procedure abs(int) to public": (
        "42809",
        "abs(integer) is not a procedure",
        None,
        None,
    ),
    "comment on procedure abs(int) is 'x'": (
        "42809",
        "abs(integer) is not a procedure",
        None,
        None,
    ),
    "comment on function abs(int) is 'x'": ("ok", None),
}


@pytest.mark.backlog("RP-519")
@pytest.mark.parametrize("query", list(BUILTIN_ROUTINES))
def test_builtin_routine_resolved_by_its_overloads(query: str, dsn: str) -> None:
    assert answer(dsn, query) == BUILTIN_ROUTINES[query]


# A signature no routine has is reported with no position.
MISSING_SIGNATURE = {
    "grant execute on function gc(bool) to public": (
        "42883",
        "function gc(boolean) does not exist",
        None,
        None,
    ),
    "grant execute on procedure gc(bool) to public": (
        "42883",
        "procedure gc(boolean) does not exist",
        None,
        None,
    ),
}


@pytest.mark.backlog("RP-520")
@pytest.mark.parametrize("query", list(MISSING_SIGNATURE))
def test_grant_missing_signature_has_no_position(query: str, dsn: str) -> None:
    assert answer(dsn, query) == MISSING_SIGNATURE[query]
