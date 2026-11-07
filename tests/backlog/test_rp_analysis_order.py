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


@pytest.mark.backlog("RP-427")
def test_insert_select_and_cte_are_type_checked(rust_pg: Any) -> None:
    """An operator with no overload for its operands' types is 42883 inside
    an INSERT's SELECT and a WITH query too, never an empty answer."""
    integer_boolean = ("err", "42883", "operator does not exist: integer = boolean")
    assert _answers(
        rust_pg,
        [
            "insert into k select * from e where a = true",
            "with t as (select * from e where a = true) select * from t",
            "explain with t as (select * from e where a = true) select * from t",
        ],
    ) == [integer_boolean, integer_boolean, integer_boolean]


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
