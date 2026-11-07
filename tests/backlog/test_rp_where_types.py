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


@pytest.mark.backlog_fixed("RP-424")
def test_where_untyped_literal_reads_as_boolean(rust_pg: Any) -> None:
    """A bare string literal is of type UNKNOWN, and `coerce_to_boolean`
    reads it as a boolean: `where 't'` kept every row and `where 'x'` was
    22P02. Both were 42804 `not type text` before RP-424."""
    assert _answers(
        rust_pg,
        ["select count(*) from e where 't'", "select count(*) from e where 'x'"],
    ) == [
        ("ok", [(10,)]),
        ("err", "22P02", 'invalid input syntax for type boolean: "x"'),
    ]


@pytest.mark.backlog_fixed("RP-424")
def test_untyped_literal_in_every_boolean_position(rust_pg: Any) -> None:
    """Every clause `coerce_to_boolean` reads -- WHERE, an argument of AND /
    NOT, a JOIN's ON, a searched CASE's WHEN, FILTER, HAVING, a subquery's
    and a CTE's WHERE, UPDATE and DELETE -- took an untyped literal as the
    boolean `boolin` makes of it (any prefix of `true` / `false` / `yes`,
    blanks trimmed), or 22P02 at the literal. A missing relation's 42P01
    still came first. Before RP-424 an ON or a CASE WHEN of `'t'` matched no
    row and each other clause was 42804 `not type text`; `'tr'::bool` was
    22P02. Each answer measured on a private PostgreSQL 15.19 cluster."""
    boolean_x = 'invalid input syntax for type boolean: "x"'
    assert _positioned(
        rust_pg,
        [
            "select count(*) from e where 'off'",
            "select count(*) from e where ' Yes '",
            "select count(*) from e where 'tr'",
            "select count(*) from e where ''",
            "select count(*) from e where 'yes' and a > 5",
            "select count(*) from e where a > 5 and 'x'",
            "select count(*) from e where not 'o'",
            "select count(*) from e join k on 't'",
            "select count(*) from e join k on 'x'",
            "select count(*) from e left join k on 'f'",
            "select sum(case when 't' then 1 else 0 end) from e",
            "select sum(case when 'x' then 1 else 0 end) from e",
            "select count(*) filter (where 'f'), count(*) filter (where 'on') from e",
            "select count(*) filter (where 'x') from e",
            "select count(*) from e having 't'",
            "select a from e group by a having 'x'",
            "select count(*) from e where exists (select 1 from k where 'x')",
            "with t as (select * from e where 'f') select count(*) from t",
            "update e set b = b where 'x'",
            "update e set b = b where 't'",
            "delete from k where 'n'",
            "select count(*) from nosuch where 'x'",
            "select count(*) from e join nosuch on 'x'",
            "select 'tr'::bool, 'of'::bool, 'ye'::bool, 'FAL'::bool",
            "select count(*) from pg_class where 'x'",
            "explain (costs off) select * from e where 't' and a > 5",
            "explain (costs off) select * from e where 'f'",
        ],
    ) == [
        ("ok", [(0,)]),
        ("ok", [(10,)]),
        ("ok", [(10,)]),
        ("err", "22P02", 'invalid input syntax for type boolean: ""', "30"),
        ("ok", [(5,)]),
        ("err", "22P02", boolean_x, "40"),
        ("err", "22P02", 'invalid input syntax for type boolean: "o"', "34"),
        ("ok", [(10,)]),
        ("err", "22P02", boolean_x, "34"),
        ("ok", [(10,)]),
        ("ok", [(10,)]),
        ("err", "22P02", boolean_x, "22"),
        ("ok", [(0, 10)]),
        ("err", "22P02", boolean_x, "31"),
        ("ok", [(10,)]),
        ("err", "22P02", boolean_x, "35"),
        ("err", "22P02", boolean_x, "60"),
        ("ok", [(0,)]),
        ("err", "22P02", boolean_x, "26"),
        ("ok", "UPDATE 10"),
        ("ok", "DELETE 0"),
        ("err", "42P01", 'relation "nosuch" does not exist', "22"),
        ("err", "42P01", 'relation "nosuch" does not exist', "29"),
        ("ok", [(True, False, True, False)]),
        ("err", "22P02", boolean_x, "37"),
        ("ok", [("Seq Scan on e",), ("  Filter: (a > 5)",)]),
        ("ok", [("Result",), ("  One-Time Filter: false",)]),
    ]


@pytest.mark.backlog("RP-429")
def test_target_list_is_analysed_before_where(rust_pg: Any) -> None:
    """`transformSelectStmt` transforms the target list before the WHERE,
    so a missing output column is 42703 ahead of any error in the WHERE;
    the FROM list, ON included, comes before both (the last case)."""
    assert _positioned(
        rust_pg,
        [
            "select nosuch from e where 'x'",
            "select nosuch from e where 1",
            "select nosuch from e where a = true",
            "select a, nosuch from e e1 join k on e1.b = 1",
        ],
    ) == [
        ("err", "42703", 'column "nosuch" does not exist', "8"),
        ("err", "42703", 'column "nosuch" does not exist', "8"),
        ("err", "42703", 'column "nosuch" does not exist', "8"),
        ("err", "42883", "operator does not exist: text = integer", "43"),
    ]


@pytest.mark.backlog("RP-433")
def test_simple_case_types_untyped_when_from_its_argument(rust_pg: Any) -> None:
    """A simple CASE compares its argument to each WHEN, so an untyped WHEN
    literal takes the argument's type: `case a when '1'` over an integer
    compares integers, and `case a when 't'` is 22P02 for type integer."""
    assert _positioned(
        rust_pg,
        [
            "select sum(case a when '1' then 1 else 0 end) from e",
            "select case 1 when '1' then 'y' else 'n' end",
            "select sum(case a when 't' then 1 else 0 end) from e",
        ],
    ) == [
        ("ok", [(1,)]),
        ("ok", [("y",)]),
        ("err", "22P02", 'invalid input syntax for type integer: "t"', "24"),
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
