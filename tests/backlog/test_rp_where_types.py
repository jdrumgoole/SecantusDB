"""Gap tests for the type checks a WHERE gets before it is planned.

PostgreSQL 15.19 refuses these at parse analysis, so neither a SELECT nor
an EXPLAIN of one reaches the planner. Each answer was measured on a private
15.19 cluster with this module's table (found while working RP-409).
"""

from __future__ import annotations

from typing import Any

import psycopg
import pytest

from tests.backlog.conftest import pg_answer
from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "create table e (a int, b text)",
    "insert into e select g, 'v' || (g % 10) from generate_series(1, 10) g",
]


def _answers(server: Any, queries: list[str]) -> list[tuple]:
    """Each query's answer, in autocommit over a fresh database."""
    with psycopg.connect(_dsn(server) + " dbname=postgres", autocommit=True) as c:
        c.execute("drop database if exists rp413_where")
        c.execute("create database rp413_where")
    with psycopg.connect(_dsn(server) + " dbname=rp413_where", autocommit=True) as c:
        for s in _SETUP:
            c.execute(s)
        return [pg_answer(c, q) for q in queries]


@pytest.mark.backlog_fixed("RP-418")
def test_where_must_be_boolean(rust_pg: Any) -> None:
    """`coerce_to_boolean`: a WHERE of another type is 42804, never rows.
    Both counted 0 before RP-418."""
    assert _answers(
        rust_pg,
        [
            "select count(*) from e where a::text",
            "select count(*) from e where null::int",
        ],
    ) == [
        (
            "err",
            "42804",
            "argument of WHERE must be type boolean, not type text",
        ),
        (
            "err",
            "42804",
            "argument of WHERE must be type boolean, not type integer",
        ),
    ]


def _positioned(server: Any, queries: list[str]) -> list[tuple]:
    """Each query's answer, an error with its 1-based `position`."""
    with psycopg.connect(_dsn(server) + " dbname=postgres", autocommit=True) as c:
        c.execute("drop database if exists rp418_where")
        c.execute("create database rp418_where")
    out: list[tuple] = []
    with psycopg.connect(_dsn(server) + " dbname=rp418_where", autocommit=True) as c:
        for s in [*_SETUP, "create table k (a int, b text)", "insert into k values (1, 'x')"]:
            c.execute(s)
        for q in queries:
            try:
                cur = c.execute(q)
                out.append(("ok", cur.fetchall() if cur.description else cur.statusmessage))
            except psycopg.Error as e:
                out.append(("err", e.sqlstate, str(e).splitlines()[0], e.diag.statement_position))
    return out


@pytest.mark.backlog_fixed("RP-418")
def test_clause_must_be_boolean_everywhere(rust_pg: Any) -> None:
    """The same check in every clause shape that reaches it -- a column, an
    expression, a function, a subquery's WHERE, an UPDATE's and a DELETE's,
    HAVING -- at the clause's leftmost token, while a boolean clause still
    answers. Each answer measured on a private PostgreSQL 15.19 cluster."""
    assert _positioned(
        rust_pg,
        [
            "select count(*) from e where a",
            "select count(*) from e where (a)::text",
            "select count(*) from e where a + 1",
            "select count(*) from e where length(b)",
            "select count(*) from e where nullif(a, 1)",
            "select count(*) from e where (select 1)",
            "select count(*) from e where exists (select 1 from k where k.a)",
            "select count(*) from e join k on e.a = k.a where k.b",
            "update e set a = a where b",
            "delete from e where a",
            "select a from e group by a having a",
            "select count(*) from e having count(*)",
            "select count(*) from e where a::text = 'v1'",
            "select count(*) from e where case when a > 1 then true end",
            "select count(*) from e where null",
            "explain (costs off) select * from e where a::text",
        ],
    ) == [
        ("err", "42804", "argument of WHERE must be type boolean, not type integer", "30"),
        ("err", "42804", "argument of WHERE must be type boolean, not type text", "31"),
        ("err", "42804", "argument of WHERE must be type boolean, not type integer", "30"),
        ("err", "42804", "argument of WHERE must be type boolean, not type integer", "30"),
        ("err", "42804", "argument of WHERE must be type boolean, not type integer", "30"),
        ("err", "42804", "argument of WHERE must be type boolean, not type integer", "30"),
        ("err", "42804", "argument of WHERE must be type boolean, not type integer", "60"),
        ("err", "42804", "argument of WHERE must be type boolean, not type text", "50"),
        ("err", "42804", "argument of WHERE must be type boolean, not type text", "26"),
        ("err", "42804", "argument of WHERE must be type boolean, not type integer", "21"),
        ("err", "42804", "argument of HAVING must be type boolean, not type integer", "35"),
        ("err", "42804", "argument of HAVING must be type boolean, not type bigint", "31"),
        ("ok", [(0,)]),
        ("ok", [(9,)]),
        ("ok", [(0,)]),
        ("err", "42804", "argument of WHERE must be type boolean, not type text", "43"),
    ]


@pytest.mark.backlog("RP-424")
def test_where_untyped_literal_reads_as_boolean(rust_pg: Any) -> None:
    """A bare string literal is of type UNKNOWN, and `coerce_to_boolean`
    reads it as a boolean: `where 't'` keeps every row and `where 'x'` is
    22P02, not 42804 `not type text`."""
    assert _answers(
        rust_pg,
        ["select count(*) from e where 't'", "select count(*) from e where 'x'"],
    ) == [
        ("ok", [(10,)]),
        ("err", "22P02", 'invalid input syntax for type boolean: "x"'),
    ]


@pytest.mark.backlog_fixed("RP-419")
def test_explain_checks_operator_types(rust_pg: Any) -> None:
    """EXPLAIN analysed its statement as the statement alone is analysed,
    and before it read an option (`transformExplainStmt` runs ahead of
    `ExplainQuery`): an operator with no overload for its argument types was
    42883 under every EXPLAIN form, and EXPLAIN ANALYZE ran no write it
    refused. Each answer was measured on a private 15.19 cluster."""
    integer_boolean = ("err", "42883", "operator does not exist: integer = boolean")
    text_integer = ("err", "42883", "operator does not exist: text = integer")
    assert _answers(
        rust_pg,
        [
            "explain (costs off) select * from e where a = true",
            "explain select * from e where b = 1",
            "explain (bogus) select * from e where a = true",
            "explain (format nope) select * from e where a = true",
            "explain (timing) select * from e where a = true",
            "explain select * from e e1 join e e2 on e1.a = e2.b",
            "explain update e set a = 1 where a = true",
            "explain analyze delete from e where b = 1",
            "select count(*) from e",
            "explain select a as x, b as x from e order by x",
            "explain (bogus) select 1/0",
        ],
    ) == [
        integer_boolean,
        text_integer,
        integer_boolean,
        integer_boolean,
        integer_boolean,
        ("err", "42883", "operator does not exist: integer = text"),
        integer_boolean,
        text_integer,
        ("ok", [(10,)]),
        ("err", "42702", 'ORDER BY "x" is ambiguous'),
        ("err", "42601", 'unrecognized EXPLAIN option "bogus"'),
    ]
