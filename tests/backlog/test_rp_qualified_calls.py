"""Gap tests of the qualified-name divergences found while working RP-485:
RP-879 (a missing function in a named schema loses the schema from its
message), RP-880 (a three-part function name is looked up by its last part),
RP-881 (CREATE TABLE does not analyse a column's DEFAULT), RP-882 (a missing
`pg_temp` relation is named by `pg_temp_N`) and RP-883 (the session's temp
namespace does not follow its transactions).

An answer is the last statement's rows, `("rowcount", n)` for a statement
with none, or `(sqlstate, message, hint, position)` for an error, the
statements run in order on one fresh session. The expected answers were
measured on a private, freshly initdb'd PostgreSQL 15.19 cluster.
"""

from __future__ import annotations

from collections.abc import Iterator
from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_NO_FUNCTION_HINT = (
    "No function matches the given name and argument types. "
    "You might need to add explicit type casts."
)


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"


def answer(dsn: str, *statements: str) -> Any:
    with psycopg.connect(dsn, autocommit=True) as c:
        out: Any = None
        for q in statements:
            try:
                cur = c.execute(q)
                out = cur.fetchall() if cur.description else ("rowcount", cur.rowcount)
            except psycopg.Error as e:
                out = (
                    e.sqlstate,
                    e.diag.message_primary,
                    e.diag.message_hint,
                    e.diag.statement_position,
                )
        return out


MISSING_FUNCTION_IN_A_SCHEMA = {
    "select public.nosuch(1)": (
        "42883",
        "function public.nosuch(integer) does not exist",
        _NO_FUNCTION_HINT,
        "8",
    ),
    "select pg_catalog.nosuch()": (
        "42883",
        "function pg_catalog.nosuch() does not exist",
        _NO_FUNCTION_HINT,
        "8",
    ),
}


@pytest.mark.backlog("RP-879")
@pytest.mark.parametrize("query", list(MISSING_FUNCTION_IN_A_SCHEMA))
def test_missing_function_keeps_its_schema(query: str, dsn: str) -> None:
    """A missing function in a named schema is named schema and all."""
    assert answer(dsn, query) == MISSING_FUNCTION_IN_A_SCHEMA[query]


@pytest.mark.backlog("RP-880")
def test_three_part_function_name(dsn: str) -> None:
    """A function named in another catalog is a cross-database reference."""
    assert answer(dsn, "select a.b.c()") == (
        "0A000",
        "cross-database references are not implemented: a.b.c",
        None,
        "8",
    )


CREATE_TABLE_DEFAULT = {
    "create table t881a (a int default nosuch_fn())": (
        "42883",
        "function nosuch_fn() does not exist",
        _NO_FUNCTION_HINT,
        "35",
    ),
    "create table t881b (a int default nosuch_schema.f())": (
        "3F000",
        'schema "nosuch_schema" does not exist',
        None,
        "35",
    ),
}


@pytest.mark.backlog("RP-881")
@pytest.mark.parametrize("statement", list(CREATE_TABLE_DEFAULT))
def test_create_table_default_is_analysed(statement: str, dsn: str) -> None:
    """A column's DEFAULT is analysed when the table is created."""
    try:
        assert answer(dsn, statement) == CREATE_TABLE_DEFAULT[statement]
    finally:
        answer(dsn, "drop table if exists t881a, t881b")


PG_TEMP_RELATION = {
    "with a temp table": ("create temp table tt882(a int)", "select * from pg_temp.nosuch"),
    "without one": ("select * from pg_temp.nosuch",),
}


@pytest.mark.backlog("RP-882")
@pytest.mark.parametrize("case", list(PG_TEMP_RELATION))
def test_pg_temp_relation_is_named_as_written(case: str, dsn: str) -> None:
    """A missing `pg_temp` relation is named `pg_temp`, at its name."""
    assert answer(dsn, *PG_TEMP_RELATION[case]) == (
        "42P01",
        'relation "pg_temp.nosuch" does not exist',
        None,
        "15",
    )


TEMP_NAMESPACE = {
    "rolled back": (
        ("begin", "create temp table tt883(a int)", "rollback", "select pg_my_temp_schema() = 0"),
        [(True,)],
    ),
    "made and dropped in one committed transaction": (
        (
            "begin",
            "create temp table tt883(a int)",
            "drop table tt883",
            "commit",
            "select pg_temp.nosuch()",
        ),
        ("42883", "function pg_temp.nosuch() does not exist", _NO_FUNCTION_HINT, "8"),
    ),
}


@pytest.mark.backlog("RP-883")
@pytest.mark.parametrize("case", list(TEMP_NAMESPACE))
def test_temp_namespace_follows_transactions(case: str, dsn: str) -> None:
    """The temp namespace is made and unmade with its transaction."""
    statements, expected = TEMP_NAMESPACE[case]
    assert answer(dsn, *statements) == expected
