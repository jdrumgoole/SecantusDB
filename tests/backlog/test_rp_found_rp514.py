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


@pytest.mark.backlog_fixed("RP-518")
def test_function_comment_seen_in_its_transaction(dsn: str) -> None:
    """A COMMENT ON FUNCTION was read back by `obj_description` in the
    transaction that made it, as a COMMENT ON TABLE already was."""
    got = answer(
        dsn,
        "comment on function gc(int) is 'w'",
        "select obj_description('gc(int)'::regprocedure, 'pg_proc')",
    )
    assert got == ("ok", [("w",)])


_GC_COMMENT = "select obj_description('gc(int)'::regprocedure, 'pg_proc')"


@pytest.mark.backlog_fixed("RP-518")
def test_function_comment_follows_its_transaction(dsn: str) -> None:
    """A COMMENT ON FUNCTION is undone by ROLLBACK TO SAVEPOINT and by
    ROLLBACK, seen by another session only once committed, and a removed
    comment likewise. Each answer measured on PostgreSQL 15.19."""

    def comment(c: psycopg.Connection) -> Any:
        return c.execute(_GC_COMMENT).fetchone()[0]

    with (
        psycopg.connect(dsn, autocommit=True) as c,
        psycopg.connect(dsn, autocommit=True) as other,
    ):
        for q in _SETUP:
            c.execute(q)
        c.execute("begin")
        c.execute("comment on function gc(int) is 'w'")
        assert (comment(c), comment(other)) == ("w", None)
        c.execute("savepoint s")
        c.execute("comment on function gc(int) is 'x'")
        assert comment(c) == "x"
        c.execute("rollback to savepoint s")
        assert comment(c) == "w"
        c.execute("rollback")
        assert comment(c) is None
        c.execute("begin")
        c.execute("comment on function gc(int) is 'z'")
        c.execute("commit")
        assert (comment(c), comment(other)) == ("z", "z")
        c.execute("begin")
        c.execute("comment on function gc(int) is null")
        assert (comment(c), comment(other)) == (None, "z")
        c.execute("commit")
        assert comment(other) is None
        # The rewritten record is still the function.
        assert c.execute("select gc(5)").fetchone() == (1,)


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
}


@pytest.mark.backlog_fixed("RP-519")
@pytest.mark.parametrize("query", list(BUILTIN_ROUTINES))
def test_builtin_routine_resolved_by_its_overloads(query: str, dsn: str) -> None:
    """A built-in routine was known by its name alone; it is now looked up
    among its `pg_proc` overloads, every one a function."""
    assert answer(dsn, query) == BUILTIN_ROUTINES[query]


@pytest.mark.backlog_fixed("RP-519")
def test_builtin_routine_takes_a_comment(dsn: str) -> None:
    """`comment on function abs(int)` was recorded, as PostgreSQL 15.19
    records it."""
    assert answer(dsn, "comment on function abs(int) is 'x'") == ("ok", None)


# Measured on PostgreSQL 15.19: the comment is a `pg_description` row of
# `pg_proc` keyed by the overload's oid, so `abs(bigint)` and `int4abs`
# (another oid) do not carry it, and IS NULL removes it.
BUILTIN_COMMENT = {
    "pg_description": (
        [
            "comment on function abs(int) is 'x'",
            "select classoid::regclass::text, objsubid, description from pg_description "
            "where objoid = 'abs(int)'::regprocedure",
        ],
        [("pg_proc", 0, "x")],
    ),
    "one overload": (
        [
            "comment on function abs(int) is 'x'",
            "select obj_description('abs(int)'::regprocedure, 'pg_proc'), "
            "obj_description('abs(bigint)'::regprocedure, 'pg_proc') is distinct from 'x', "
            "obj_description('int4abs(int)'::regprocedure, 'pg_proc') is distinct from 'x'",
        ],
        [("x", True, True)],
    ),
    "polymorphic": (
        [
            "comment on function array_length(anyarray, int) is 'z'",
            "select obj_description('array_length(anyarray,int)'::regprocedure, 'pg_proc')",
        ],
        [("z",)],
    ),
    "removed": (
        [
            "comment on function abs(int) is 'x'",
            "comment on function abs(int) is null",
            "select count(*) from pg_description where objoid = 'abs(int)'::regprocedure "
            "and description = 'x'",
        ],
        [(0,)],
    ),
}


@pytest.mark.backlog_fixed("RP-519")
@pytest.mark.parametrize("case", list(BUILTIN_COMMENT))
def test_builtin_routine_comment_in_pg_description(case: str, dsn: str) -> None:
    queries, rows = BUILTIN_COMMENT[case]
    assert answer(dsn, *queries) == ("ok", rows)


@pytest.mark.backlog_fixed("RP-519")
def test_builtin_routine_comment_rolls_back(dsn: str) -> None:
    answer(dsn, "comment on function abs(int) is 'x'")
    with psycopg.connect(dsn, autocommit=True) as c:
        got = c.execute(
            "select count(*) from pg_description where objoid = "
            "'abs(int)'::regprocedure and description = 'x'"
        ).fetchone()
    assert got == (0,)


# A signature no routine has was reported with no position, as
# `LookupFuncWithArgs` reports it.
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
    "revoke execute on function gc(bool) from public": (
        "42883",
        "function gc(boolean) does not exist",
        None,
        None,
    ),
    "revoke execute on procedure gc(bool) from public": (
        "42883",
        "procedure gc(boolean) does not exist",
        None,
        None,
    ),
}


@pytest.mark.backlog_fixed("RP-520")
@pytest.mark.parametrize("query", list(MISSING_SIGNATURE))
def test_grant_missing_signature_has_no_position(query: str, dsn: str) -> None:
    assert answer(dsn, query) == MISSING_SIGNATURE[query]


# The comments the planner answers `obj_description` from were read outside
# the open block, so a COMMENT ON INDEX was not seen in the transaction
# that made it (found while working RP-519).
@pytest.mark.backlog_fixed("RP-519")
def test_index_comment_seen_in_its_transaction(dsn: str) -> None:
    got = answer(
        dsn,
        "create table rq519t (a int)",
        "create index rq519ix on rq519t (a)",
        "commit",
        "begin",
        "comment on index rq519ix is 'i'",
        "select obj_description('rq519ix'::regclass, 'pg_class')",
    )
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute("drop table if exists rq519t")
    assert got == ("ok", [("i",)])


# initdb's descriptions of the built-in routines (`pg_description` from
# `pg_proc.dat`), measured on PostgreSQL 15.19.
INITDB_DESCRIPTIONS = {
    "abs(int)": "absolute value",
    "int4abs(integer)": "implementation of @ operator",
    "array_length(anyarray,int)": "array length",
}


@pytest.mark.backlog_fixed("RP-969")
@pytest.mark.parametrize("routine", list(INITDB_DESCRIPTIONS))
def test_builtin_routine_has_initdb_description(routine: str, dsn: str) -> None:
    got = answer(dsn, f"select obj_description('{routine}'::regprocedure, 'pg_proc')")
    assert got == ("ok", [(INITDB_DESCRIPTIONS[routine],)])


@pytest.mark.backlog_fixed("RP-969")
def test_comment_replaces_and_removes_initdb_description(dsn: str) -> None:
    # COMMENT replaces initdb's row and IS NULL deletes it, initdb's
    # included (`DeleteComments`), on PostgreSQL 15.19.
    q = "select coalesce(obj_description('abs(int)'::regprocedure, 'pg_proc'), '<null>')"
    with psycopg.connect(dsn) as c:
        c.execute("comment on function abs(int) is 'mine'")
        assert c.execute(q).fetchall() == [("mine",)]
        c.execute("comment on function abs(int) is null")
        assert c.execute(q).fetchall() == [("<null>",)]
        c.rollback()
    with psycopg.connect(dsn) as c:
        assert c.execute(q).fetchall() == [("absolute value",)]


# initdb's descriptions of the other built-in objects (`pg_type.dat`,
# `pg_operator.dat`, `pg_am.dat`, ...), measured on PostgreSQL 15.19.
INITDB_OTHER_DESCRIPTIONS = {
    "select obj_description('int4'::regtype, 'pg_type')": (
        "-2 billion to 2 billion integer, 4-byte storage"
    ),
    "select obj_description('+(int4,int4)'::regoperator, 'pg_operator')": "add",
    "select obj_description(oid, 'pg_am') from pg_am where amname = 'btree'": (
        "b-tree index access method"
    ),
}


@pytest.mark.backlog_fixed("RP-969")
@pytest.mark.parametrize("query", list(INITDB_OTHER_DESCRIPTIONS))
def test_builtin_object_has_initdb_description(query: str, dsn: str) -> None:
    """A built-in type, operator and access method had no description:
    initdb's are served from `pg15_other_descriptions.tsv`."""
    assert answer(dsn, query) == ("ok", [(INITDB_OTHER_DESCRIPTIONS[query],)])


# COMMENT ON a built-in object of another catalog replaces (IS NULL
# deletes) initdb's description; measured on PostgreSQL 15.19.
COMMENT_ON_BUILTIN = {
    "type": (
        "comment on type int4 is 'x'",
        "select obj_description('int4'::regtype, 'pg_type')",
        "x",
    ),
    "operator, is null": (
        "comment on operator +(int4,int4) is null",
        "select obj_description('+(int4,int4)'::regoperator, 'pg_operator')",
        None,
    ),
    "access method": (
        "comment on access method btree is 'y'",
        "select obj_description(403, 'pg_am')",
        "y",
    ),
}


@pytest.mark.backlog("RP-969")
@pytest.mark.parametrize("case", list(COMMENT_ON_BUILTIN))
def test_comment_on_builtin_object_replaces_initdb_description(case: str, dsn: str) -> None:
    comment, query, want = COMMENT_ON_BUILTIN[case]
    assert answer(dsn, comment, query) == ("ok", [(want,)])
