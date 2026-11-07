"""Gap tests for what PostgreSQL refuses at parse analysis, before planning.

Each answer was measured on a private PostgreSQL 15.19 cluster with this
module's tables (found while working RP-419).
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
    "create table k (a int primary key, b text)",
]


def _answers(server: Any, queries: list[str]) -> list[tuple]:
    """Each query's answer, in autocommit over a fresh database."""
    with psycopg.connect(_dsn(server) + " dbname=postgres", autocommit=True) as c:
        c.execute("drop database if exists rp427_analysis")
        c.execute("create database rp427_analysis")
    with psycopg.connect(_dsn(server) + " dbname=rp427_analysis", autocommit=True) as c:
        for s in _SETUP:
            c.execute(s)
        return [pg_answer(c, q) for q in queries]


@pytest.mark.backlog_fixed("RP-427")
def test_insert_select_and_cte_are_type_checked(rust_pg: Any) -> None:
    """An operator with no overload for its operands' types was an empty
    answer inside an INSERT's SELECT and a WITH query; it is 42883 there
    too, as `transformWithClause` analyses every CTE's query, used or not,
    and an UPDATE or DELETE's WITH as a SELECT's."""
    integer_boolean = ("err", "42883", "operator does not exist: integer = boolean")
    text_integer = ("err", "42883", "operator does not exist: text = integer")
    assert _answers(
        rust_pg,
        [
            "insert into k select * from e where a = true",
            "with t as (select * from e where a = true) select * from t",
            "explain with t as (select * from e where a = true) select * from t",
            "with t as (select * from e where b = 1) select 1",
            "with t as (select b as a from e), u as (select * from t where a = 1) select 1",
            "insert into k with t as (select a, b from e) select * from t where a = true",
            "with t as (select * from e where a = true) update k set b = 'x'",
            "with t as (select * from e where a = true) delete from k",
            "with x as (update k set b = 'q' where a = true returning *) select * from x",
            "with t as (select a, b from e) insert into k select * from t where a > 8",
            "with recursive r(n) as (select 1 union all select n + 1 from r where n < 3)"
            " select * from r",
        ],
    ) == [
        integer_boolean,
        integer_boolean,
        integer_boolean,
        text_integer,
        text_integer,
        integer_boolean,
        integer_boolean,
        integer_boolean,
        integer_boolean,
        ("ok", "INSERT 0 2"),
        ("ok", [(1,), (2,), (3,)]),
    ]


@pytest.mark.backlog("RP-431")
def test_star_derived_table_is_typed(rust_pg: Any) -> None:
    """A FROM subquery or CTE written `select *` has its relation's column
    types, so a comparison over them is 42883 at parse analysis."""
    integer_boolean = ("err", "42883", "operator does not exist: integer = boolean")
    assert _answers(
        rust_pg,
        [
            "select * from (select * from e) t where a = true",
            "with t as (select * from e) select * from t where a = true",
            "with t as (select * from e) insert into k select * from t where a = true",
        ],
    ) == [integer_boolean, integer_boolean, integer_boolean]


@pytest.mark.backlog("RP-432")
def test_update_and_delete_read_their_ctes(rust_pg: Any) -> None:
    """An UPDATE or DELETE reads the CTEs of its own WITH, in FROM / USING
    and in a subquery, and so does a data-modifying CTE the ones before it."""
    assert _answers(
        rust_pg,
        [
            "insert into k values (1, 'v1'), (2, 'x'), (50, 'w')",
            "with t as (select b from e) update k set a = a + 100 from t where k.b = t.b",
            "with t as (select a from e) update k set b = 'y' where a in (select a from t)",
            "with t as (select 50 as a) delete from k using t where k.a = t.a",
            "with t as (select 2 as a), u as (delete from k where a in (select a from t)"
            " returning a) select * from u",
            "select a, b from k order by a",
        ],
    ) == [
        ("ok", "INSERT 0 3"),
        ("ok", "UPDATE 1"),
        ("ok", "UPDATE 1"),
        ("ok", "DELETE 1"),
        ("ok", [(2,)]),
        ("ok", [(101, "v1")]),
    ]


@pytest.mark.backlog("RP-428")
def test_explain_resolves_names_before_options(rust_pg: Any) -> None:
    """`transformExplainStmt` resolves the statement's relations, functions
    and literals before `ExplainQuery` reads an option."""
    assert _answers(
        rust_pg,
        [
            "explain (bogus) select * from nosuch",
            "explain (bogus) select nosuchfn(a) from e",
            "explain (bogus) select * from e where a = 'x'",
            "explain (bogus) select 1/0",
        ],
    ) == [
        ("err", "42P01", 'relation "nosuch" does not exist'),
        ("err", "42883", "function nosuchfn(integer) does not exist"),
        ("err", "22P02", 'invalid input syntax for type integer: "x"'),
        ("err", "42601", 'unrecognized EXPLAIN option "bogus"'),
    ]
