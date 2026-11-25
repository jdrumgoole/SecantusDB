"""Gap tests of the divergences found while working RP-1050 (a UNION's
unmatched column types under `INSERT ... SELECT` had no position; fixed): RP-1087
(the mismatch is placed at the wrong column or arm of a plain query),
RP-1088 (the `INSERT` checks its column's assignment before the set
operation's types) and RP-1089 (a statement that only analyses its query,
`EXPLAIN` or `PREPARE`, does not check the set operation's types at all).

An answer is `(sqlstate, message, position)` for an error, or the command
tag. The expected answers were measured on PostgreSQL 15.19; the tests that
need no objects need no setup, and the `INSERT` one runs after
`test_rp_found_rp552`'s `_SETUP`.
"""

from __future__ import annotations

from collections.abc import Iterator
from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn
from tests.backlog.test_rp_found_rp552 import _SETUP


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        for q in _SETUP[:5]:
            c.execute(q)


def answer(dsn: str, query: str, setup: list[str] | None = None) -> Any:
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute("deallocate all")
        for q in setup or []:
            c.execute(q)
        try:
            cur = c.execute(query)
        except psycopg.Error as e:
            return (e.sqlstate, e.diag.message_primary, e.diag.statement_position)
        return cur.statusmessage


# `transformSetOperationTree` checks the left subtree, then the right, then
# this node's columns in order, and `select_common_type` points at the
# right-hand column of the first pair that does not match.
PLACED = {
    "select 1, 'a'::text union select 2, 3": "37",
    "select 1 union select 'a'::text union select 2": "23",
    "select 1 union (select 'a'::text union select 'b'::text)": "24",
}


@pytest.mark.backlog("RP-1087")
@pytest.mark.parametrize("query", list(PLACED))
def test_set_op_mismatch_column_and_arm(dsn: str, query: str) -> None:
    """The mismatch is at the right column of the first unmatched pair."""
    got = answer(dsn, query)
    assert got[0] == "42804" and got[2] == PLACED[query], got


@pytest.mark.backlog("RP-1088")
def test_insert_select_set_op_types_before_assignment(dsn: str) -> None:
    """The query's set operation is analysed before the INSERT assigns it."""
    assert answer(dsn, "insert into tct(c) select null::text union select 1", _SETUP) == (
        "42804",
        "UNION types text and integer cannot be matched",
        "51",
    )


# Parse analysis checks the set operation's types, so a statement that only
# analyses its query refuses it too.
ANALYSED = {
    "explain select null::text union select 1": "40",
    "prepare p1050d as select null::text union select 1": "50",
    "create view v1050d as select null::text union select 1": "54",
}


@pytest.mark.backlog("RP-1089")
@pytest.mark.parametrize("query", list(ANALYSED))
def test_set_op_mismatch_refused_at_analysis(dsn: str, query: str) -> None:
    """EXPLAIN and PREPARE refuse the unmatched set operation, positioned."""
    assert answer(dsn, query) == (
        "42804",
        "UNION types text and integer cannot be matched",
        ANALYSED[query],
    )
