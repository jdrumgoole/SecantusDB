"""Gap tests of the divergences found while working RP-1089 (a set
operation's unmatched column types were not refused by a statement that only
analyses its query; fixed): a set operation's untyped literal is read as
the other side's type only when the set operation runs, and `CREATE RULE`
on a missing relation reports a position PostgreSQL does not.

An answer is `(sqlstate, message, position)` for an error, or the command
tag. The expected answers were measured on PostgreSQL 15.20, and none needs
an object.
"""

from __future__ import annotations

from collections.abc import Iterator
from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        c.execute("drop view if exists v1089n")


def answer(dsn: str, query: str) -> Any:
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute("deallocate all")
        try:
            cur = c.execute(query)
        except psycopg.Error as e:
            return (e.sqlstate, e.diag.message_primary, e.diag.statement_position)
        return cur.statusmessage


# `transformSetOperationTree` coerces an `unknown` Const side to the
# column's common type, which reads it with the type's input function.
LITERAL = {
    "explain select 'a' union select 1": "16",
    "prepare p1089n as select 'a' union select 1": "26",
    "create view v1089n as select 'a' union select 1": "30",
}


@pytest.mark.backlog("RP-1216")
@pytest.mark.parametrize("query", list(LITERAL))
def test_set_op_untyped_literal_read_at_analysis(dsn: str, query: str) -> None:
    """The untyped literal is read as integer when the query is analysed."""
    assert answer(dsn, query) == (
        "22P02",
        'invalid input syntax for type integer: "a"',
        LITERAL[query],
    )


@pytest.mark.backlog("RP-1217")
def test_create_rule_missing_relation_unpositioned(dsn: str) -> None:
    """`DefineRule` opens the relation by name: no position."""
    assert answer(dsn, "create rule r1089 as on insert to nosuch1089 do instead nothing") == (
        "42P01",
        'relation "nosuch1089" does not exist',
        None,
    )
