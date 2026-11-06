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


@pytest.mark.backlog("RP-413")
def test_where_must_be_boolean(rust_pg: Any) -> None:
    """`coerce_to_boolean`: a WHERE of another type is 42804, never rows."""
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


@pytest.mark.backlog("RP-414")
def test_explain_checks_operator_types(rust_pg: Any) -> None:
    """EXPLAIN analyses its statement as the statement alone is analysed:
    an operator with no overload for its argument types is 42883."""
    assert _answers(rust_pg, ["explain (costs off) select * from e where a = true"]) == [
        ("err", "42883", "operator does not exist: integer = boolean"),
    ]
