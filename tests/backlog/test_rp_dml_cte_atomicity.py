"""Gap test of RP-503, found while working RP-501: a data-modifying WITH item's write
survives the error of the statement it is part of.

PostgreSQL runs a statement as one unit: when any part of it fails, every
write it made is undone, the writes of its data-modifying WITH items
included. Here such an item is run (`materialize_dml_ctes`) before the rest
of the statement is planned, and its write stays committed when the rest
then fails, in autocommit -- a statement reported as failed has deleted,
inserted or updated rows.

Measured on a private PostgreSQL 15.19 cluster: the error, then the
table's rows.
"""

from __future__ import annotations

from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

SETUP = [
    "drop table if exists rp501_w",
    "create table rp501_w (a int)",
    "insert into rp501_w values (1), (2)",
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


@pytest.mark.backlog("RP-503")
@pytest.mark.parametrize("query", list(FAILING_STATEMENTS))
def test_failed_statement_keeps_no_write(query: str, rust_pg: Any) -> None:
    """The error matches; the rows the item wrote stay written here (the
    table holds `[]`, `[1, 2, 7]` or `[11, 12]`)."""
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        for statement in SETUP:
            c.execute(statement)
        try:
            cur = c.execute(query)
            got: object = cur.statusmessage
        except psycopg.Error as e:
            got = (e.sqlstate, e.diag.message_primary, e.diag.statement_position)
        rows = [r[0] for r in c.execute("select a from rp501_w order by 1").fetchall()]
    assert (got, rows) == FAILING_STATEMENTS[query]
