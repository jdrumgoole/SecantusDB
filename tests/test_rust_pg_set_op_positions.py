"""Where the Rust PostgreSQL server places a set operation's analysis
errors: `transformSetOperationTree` checks the left subtree, then the right,
then this node's columns in order, and points each error at a column's
`exprLocation` -- a leaf side's target, or the expression a nested set
operation took its common type from (RP-1087).

Measured on PostgreSQL 15.19.
"""

from __future__ import annotations

from collections.abc import Iterator
from typing import Any

import psycopg
import pytest

from tests.test_rust_pgserver_slice import BINARY, _Server

pytestmark = pytest.mark.skipif(not BINARY.exists(), reason="secantusd-pg not built")


@pytest.fixture(scope="module")
def conn(tmp_path_factory: pytest.TempPathFactory) -> Iterator[Any]:
    with _Server(tmp_path_factory.mktemp("setop")) as server, server.connect() as c:
        c.autocommit = True
        yield c


def answer(conn: Any, query: str) -> Any:
    try:
        cur = conn.execute(query)
    except psycopg.Error as e:
        return (e.sqlstate, e.diag.message_primary, e.diag.statement_position)
    return cur.statusmessage


MISMATCH = {
    # A nested side's column is where its common type came from: the right
    # side's when the left is untyped, the left's otherwise.
    "select 1 union (select null union select 'a'::text)": "42",
    "select 'x'::text union (select 1 union select 2::int8)": "47",
    "select 'x'::text union (select 1::int8 union select 2)": "32",
    # Two untyped sides resolve as text, below the top.
    "select null union select null union select 1": "44",
    # A cast of an untyped literal is the literal; any other cast is its
    # leftmost token.
    "select 1 union select cast(null as text)": "28",
    "select 1 union select cast(1 as text)": "23",
    "select 1 union select text 'a'": "28",
    "select 1 union select 'a'::text || 'b'": "23",
    "select 1 union select * from (select 'a'::text) s": "23",
    # A VALUES side's columns have no location.
    "select 1 union values ('a'::text)": None,
    "values (1) union select 'a'::text": "25",
    "select * from (select 1 union select 'a'::text) s": "38",
    "with c as (select 1 union select 'a'::text) select * from c": "34",
}


@pytest.mark.parametrize("query", list(MISMATCH))
def test_set_op_mismatch_position(conn: Any, query: str) -> None:
    got = answer(conn, query)
    assert got[0] == "42804" and got[2] == MISMATCH[query], got


# `each UNION query must have the same number of columns` is at
# `exprLocation` of the right side's target list, its first column with a
# location; `coerce_to_common_type` refuses the side being converted.
OTHER = {
    "select 1 union select 'a'::text, 2": (
        "42601",
        "each UNION query must have the same number of columns",
        "23",
    ),
    "select 1, 2 union (select 3 union select 4)": (
        "42601",
        "each UNION query must have the same number of columns",
        "27",
    ),
    "select 1, 2 union values (3)": (
        "42601",
        "each UNION query must have the same number of columns",
        None,
    ),
    "select 1.5 union select 1::oid": (
        "42846",
        "UNION could not convert type oid to numeric",
        "25",
    ),
    "select 1::oid union select 1.5": (
        "42846",
        "UNION could not convert type numeric to oid",
        "28",
    ),
}


@pytest.mark.parametrize("query", list(OTHER))
def test_set_op_column_count_and_conversion_positions(conn: Any, query: str) -> None:
    assert answer(conn, query) == OTHER[query]


# Parse analysis makes these checks, so a statement that only analyses its
# query refuses the set operation too, before anything runs: `EXPLAIN` with
# or without ANALYZE, `PREPARE`, and `CREATE VIEW`, whose query is checked
# as the client wrote it (RP-1089). Measured on PostgreSQL 15.20.
ANALYSED = {
    "explain select null::text union select 1": (
        "42804",
        "UNION types text and integer cannot be matched",
        "40",
    ),
    "explain (analyze) select null::text union select 1": (
        "42804",
        "UNION types text and integer cannot be matched",
        "50",
    ),
    "explain select 1 union select 'a'::text union select 2": (
        "42804",
        "UNION types integer and text cannot be matched",
        "31",
    ),
    "explain select 1 intersect select now()": (
        "42804",
        "INTERSECT types integer and timestamp with time zone cannot be matched",
        "35",
    ),
    "explain select 1, 2 union select 3": (
        "42601",
        "each UNION query must have the same number of columns",
        "34",
    ),
    "explain values (1) union select 'x'::text": (
        "42804",
        "UNION types integer and text cannot be matched",
        "33",
    ),
    "prepare p1089a as select null::text union select 1": (
        "42804",
        "UNION types text and integer cannot be matched",
        "50",
    ),
    "prepare p1089b as select 1, 2 union select 3": (
        "42601",
        "each UNION query must have the same number of columns",
        "44",
    ),
    "prepare p1089c as select 1::numeric union select 2::oid": (
        "42846",
        "UNION could not convert type oid to numeric",
        "50",
    ),
    "create view   v1089a as select null::text union select 1": (
        "42804",
        "UNION types text and integer cannot be matched",
        "56",
    ),
    "create view v1089b as select 1, 2 union select 3": (
        "42601",
        "each UNION query must have the same number of columns",
        "48",
    ),
    "create view v1089c as select 1 union select 2::oid union select 'x'::text": (
        "42804",
        "UNION types oid and text cannot be matched",
        "65",
    ),
    "create view v1089d as select 1::numeric union select 2::oid": (
        "42846",
        "UNION could not convert type oid to numeric",
        "54",
    ),
    "create view v1089e as select case when true then 1 else 'a'::text end": (
        "42804",
        "CASE types text and integer cannot be matched",
        "50",
    ),
    "select 1; create view v1089f as select 1, 'a'::text union select 2, 3": (
        "42804",
        "UNION types text and integer cannot be matched",
        "69",
    ),
    # What analysis accepts still succeeds.
    "explain select 1 union select 2.5": "EXPLAIN",
    "explain select null union select 1": "EXPLAIN",
}


@pytest.mark.parametrize("query", list(ANALYSED))
def test_set_op_refused_at_analysis(conn: Any, query: str) -> None:
    assert answer(conn, query) == ANALYSED[query]
