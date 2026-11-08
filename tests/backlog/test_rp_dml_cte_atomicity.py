"""Regression test of RP-503, found while working RP-501: a data-modifying
WITH item's write survived the error of the statement it was part of.

PostgreSQL runs a statement as one unit: when any part of it fails, every
write it made is undone, the writes of its data-modifying WITH items
included. Here such an item is run (`materialize_dml_ctes`) while the
statement is planned, before the rest of it runs, and a lone statement in
autocommit sent by the simple protocol had no transaction of its own: the
item's write committed, and a statement reported as failed had deleted,
inserted or updated rows. Fixed by giving such a statement its implicit
transaction (`runs_user_code`, `has_data_modifying_with`), as a statement
that runs user code already had.

Measured on a private PostgreSQL 15.19 cluster: the error, then the
tables' rows, by the simple protocol and by a prepared statement alike.
"""

from __future__ import annotations

from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

SETUP = [
    "drop table if exists rp501_w",
    "drop table if exists rp501_u",
    "create table rp501_w (a int)",
    "create table rp501_u (b int)",
    "insert into rp501_w values (1), (2)",
    "insert into rp501_u values (5)",
]
FAILING_STATEMENTS: dict[str, object] = {
    "with x as (delete from rp501_w returning a) select nosuch from x": (
        ("42703", 'column "nosuch" does not exist', "52"),
        [1, 2],
    ),
    "with x as (insert into rp501_w values (7) returning a) select 1/0 from x": (
        ("22012", "division by zero", None),
        [1, 2],
    ),
    "with x as (update rp501_w set a = a + 10 returning a) select nosuch from x": (
        ("42703", 'column "nosuch" does not exist', "62"),
        [1, 2],
    ),
    "with x as (delete from rp501_w returning a), w as (select nosuch) select 1": (
        ("42703", 'column "nosuch" does not exist', "59"),
        [1, 2],
    ),
}
# The statement the item belongs to is itself a write that fails: neither
# table changes.
FAILING_WRITES: dict[str, object] = {
    "with x as (delete from rp501_w returning a) insert into rp501_u select 10/(a-1) from x": (
        ("22012", "division by zero", None),
        [1, 2],
        [5],
    ),
    "with x as (delete from rp501_w where a = 1 returning a) update rp501_u set b = 1/0": (
        ("22012", "division by zero", None),
        [1, 2],
        [5],
    ),
}
# A statement that succeeds keeps its item's write, and its own.
SUCCEEDING: dict[str, object] = {
    "with x as (delete from rp501_w returning a) select * from x": ("SELECT 2", [], [5]),
    "with x as (insert into rp501_w values (7) returning a) insert into rp501_u select a from x": (
        "INSERT 0 1",
        [1, 2, 7],
        [5, 7],
    ),
}


def _run(rust_pg: Any, query: str, prepare: bool) -> tuple[object, list[int], list[int]]:
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        for statement in SETUP:
            c.execute(statement)
        try:
            cur = c.execute(query, prepare=prepare)
            got: object = cur.statusmessage
        except psycopg.Error as e:
            got = (e.sqlstate, e.diag.message_primary, e.diag.statement_position)
        w = [r[0] for r in c.execute("select a from rp501_w order by 1").fetchall()]
        u = [r[0] for r in c.execute("select b from rp501_u order by 1").fetchall()]
    return got, w, u


@pytest.mark.backlog_fixed("RP-503")
@pytest.mark.parametrize("query", list(FAILING_STATEMENTS))
def test_failed_statement_keeps_no_write(query: str, rust_pg: Any) -> None:
    """The error matched, and the rows the item wrote were taken back (the
    table held `[]`, `[1, 2, 7]` or `[11, 12]` here)."""
    got, w, _ = _run(rust_pg, query, prepare=False)
    assert (got, w) == FAILING_STATEMENTS[query]


@pytest.mark.backlog_fixed("RP-503")
@pytest.mark.parametrize("prepare", [False, True], ids=["simple", "prepared"])
@pytest.mark.parametrize("query", list(FAILING_WRITES) + list(SUCCEEDING))
def test_statement_and_its_items_write_as_one(query: str, prepare: bool, rust_pg: Any) -> None:
    """A failing INSERT or UPDATE took its item's write back with its own;
    a statement that succeeded kept both."""
    expected = FAILING_WRITES.get(query) or SUCCEEDING[query]
    assert _run(rust_pg, query, prepare) == expected
