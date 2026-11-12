"""Gap tests of the qualified-name divergences found while working RP-485:
RP-879 (a missing function in a named schema loses the schema from its
message), RP-880 (a three-part function name is looked up by its last part),
RP-881 (CREATE TABLE did not analyse a column's DEFAULT; fixed), RP-882 (a missing
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


@pytest.mark.backlog_fixed("RP-881")
@pytest.mark.parametrize("statement", list(CREATE_TABLE_DEFAULT))
def test_create_table_default_is_analysed(statement: str, dsn: str) -> None:
    """A column's DEFAULT was analysed only when an INSERT first used it;
    now it is analysed when the table is created."""
    try:
        assert answer(dsn, statement) == CREATE_TABLE_DEFAULT[statement]
    finally:
        answer(dsn, "drop table if exists t881a, t881b")


# What `cookDefault`'s analysis (`EXPR_KIND_COLUMN_DEFAULT`) refuses, in the
# order it meets it, for each statement that declares a default: the
# statements, then the last one's answer, measured on a private 15.19
# cluster. ALTER TABLE and the domain statements report no position.
DEFAULT_ANALYSIS: dict[str, tuple[tuple[str, ...], Any]] = {
    "a column reference": (
        ("create table t881c (a int, b int default a)",),
        ("0A000", "cannot use column reference in DEFAULT expression", None, "42"),
    ),
    "a qualified column reference": (
        ("create table t881c (a int default t881c.a)",),
        ("0A000", "cannot use column reference in DEFAULT expression", None, "35"),
    ),
    "a subquery": (
        ("create table t881c (a int default (select 1))",),
        ("0A000", "cannot use subquery in DEFAULT expression", None, "35"),
    ),
    "an EXISTS": (
        ("create table t881c (a int default exists (select 1))",),
        ("0A000", "cannot use subquery in DEFAULT expression", None, "35"),
    ),
    "an aggregate": (
        ("create table t881c (a int default sum(1))",),
        ("42803", "aggregate functions are not allowed in DEFAULT expressions", None, "35"),
    ),
    "count(*)": (
        ("create table t881c (a int default count(*))",),
        ("42803", "aggregate functions are not allowed in DEFAULT expressions", None, "35"),
    ),
    "a set-returning function": (
        ("create table t881c (a int default generate_series(1, 2))",),
        ("0A000", "set-returning functions are not allowed in DEFAULT expressions", None, "35"),
    ),
    "a window function": (
        ("create table t881c (a int default row_number() over ())",),
        ("42P20", "window functions are not allowed in DEFAULT expressions", None, "35"),
    ),
    "a window function with no OVER": (
        ("create table t881c (a int default row_number())",),
        ("42809", "window function row_number requires an OVER clause", None, "35"),
    ),
    "OVER on a plain function": (
        ("create table t881c (a int default abs(1) over ())",),
        (
            "42809",
            "OVER specified, but abs is not a window function nor an aggregate function",
            None,
            "35",
        ),
    ),
    "the arguments first": (
        ("create table t881c (a int default abs(nosuch_fn()))",),
        (
            "42883",
            "function nosuch_fn() does not exist",
            _NO_FUNCTION_HINT,
            "39",
        ),
    ),
    "an aggregate's arguments first": (
        ("create table t881c (a int default sum(nosuch_fn()))",),
        (
            "42883",
            "function nosuch_fn() does not exist",
            _NO_FUNCTION_HINT,
            "39",
        ),
    ),
    "a reference in the arguments": (
        ("create table t881c (a int default nosuch_fn(b))",),
        ("0A000", "cannot use column reference in DEFAULT expression", None, "45"),
    ),
    "left before right": (
        ("create table t881c (a int default nosuch_fn() + b)",),
        (
            "42883",
            "function nosuch_fn() does not exist",
            _NO_FUNCTION_HINT,
            "35",
        ),
    ),
    "a type error left of a missing call": (
        ("create table t881c (a int default lower(1) + nosuch_fn())",),
        (
            "42883",
            "function lower(integer) does not exist",
            _NO_FUNCTION_HINT,
            "35",
        ),
    ),
    "the argument types": (
        ("create table t881c (a int default nosuch_fn(1, 'x'))",),
        (
            "42883",
            "function nosuch_fn(integer, unknown) does not exist",
            _NO_FUNCTION_HINT,
            "35",
        ),
    ),
    "before the CHECKs": (
        ("create table t881c (a int check (nosuch_fn() > 0) default nosuch_g())",),
        (
            "42883",
            "function nosuch_g() does not exist",
            _NO_FUNCTION_HINT,
            "59",
        ),
    ),
    "after the column types": (
        ("create table t881c (a int default nosuch_fn(), b nosuchtype)",),
        ("42704", 'type "nosuchtype" does not exist', None, "50"),
    ),
    "after the column names": (
        ("create table t881c (a int default nosuch_fn(), a int)",),
        ("42701", 'column "a" specified more than once', None, None),
    ),
    "beside a serial": (
        ("create table t881c (a serial, b int default nosuch_fn())",),
        (
            "42883",
            "function nosuch_fn() does not exist",
            _NO_FUNCTION_HINT,
            "45",
        ),
    ),
    "IF NOT EXISTS over a table that exists": (
        (
            "create table t881c (a int)",
            "create table if not exists t881c (a int default nosuch_fn())",
        ),
        ("rowcount", -1),
    ),
    "ALTER TABLE ADD COLUMN": (
        ("create table t881c (a int)", "alter table t881c add column b int default (select 1)"),
        ("0A000", "cannot use subquery in DEFAULT expression", None, None),
    ),
    "ALTER TABLE SET DEFAULT": (
        ("create table t881c (a int)", "alter table t881c alter column a set default nosuch_fn()"),
        (
            "42883",
            "function nosuch_fn() does not exist",
            _NO_FUNCTION_HINT,
            None,
        ),
    ),
    "a partition's column": (
        (
            "create table t881c (a int, b int) partition by range (a)",
            "create table t881d partition of t881c (b default nosuch_fn())"
            " for values from (1) to (10)",
        ),
        (
            "42883",
            "function nosuch_fn() does not exist",
            _NO_FUNCTION_HINT,
            "50",
        ),
    ),
    "CREATE DOMAIN": (
        ("create domain d881 as int default nosuch_fn()",),
        (
            "42883",
            "function nosuch_fn() does not exist",
            _NO_FUNCTION_HINT,
            None,
        ),
    ),
    "ALTER DOMAIN SET DEFAULT": (
        ("create domain d881 as int", "alter domain d881 set default sum(1)"),
        ("42803", "aggregate functions are not allowed in DEFAULT expressions", None, None),
    ),
    "a default that analyses": (
        (
            "create table t881c"
            " (a int default abs(-1) + length('xy'), b timestamptz default now())",
            "insert into t881c default values",
            "select a, b is not null from t881c",
        ),
        [(3, True)],
    ),
}


@pytest.mark.backlog_fixed("RP-881")
@pytest.mark.parametrize("case", list(DEFAULT_ANALYSIS))
def test_default_is_analysed_as_cook_default_does(case: str, dsn: str) -> None:
    """A DEFAULT was taken unanalysed by every statement that declares one;
    now each refuses what PostgreSQL's analysis of a default refuses, at
    the token it meets first, and an IF NOT EXISTS that skips analyses
    nothing."""
    statements, expected = DEFAULT_ANALYSIS[case]
    try:
        assert answer(dsn, *statements) == expected
    finally:
        answer(dsn, "drop table if exists t881c, t881d cascade", "drop domain if exists d881")


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
