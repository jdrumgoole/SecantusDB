"""What working RP-849 (rows of zero length or unequal lengths) found:

- a row beside an operand that is not a row looks its operator up before
  the operands are transformed, so a column nothing has is 42883 `operator
  does not exist: record = integer`, not 42703.

An answer is the rows and the column names, or `(sqlstate, message,
position)` for an error. Measured on PostgreSQL 15.19 (the shared reference
on port 5415): every statement is VALUES-only.
"""

from __future__ import annotations

from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> str:
    return _dsn(rust_pg) + " dbname=postgres"


def answer(dsn: str, query: str) -> Any:
    with psycopg.connect(dsn, autocommit=True) as c:
        try:
            cur = c.execute(query)
        except psycopg.Error as e:
            return (e.sqlstate, e.diag.message_primary, e.diag.statement_position)
        return (cur.fetchall(), [d.name for d in cur.description or []])


# `transformAExprOp` transforms both operands before `make_op` looks the
# operator up, so the missing column is the error first. Here 42883.
ROW_BESIDE_NON_ROW = {
    "select (1, nosuch) = (1) from (values (1)) v(a)": (
        "42703",
        'column "nosuch" does not exist',
        "12",
    ),
    "select (1, nosuch) in (1, 2) from (values (1)) v(a)": (
        "42703",
        'column "nosuch" does not exist',
        "12",
    ),
}


@pytest.mark.backlog("RP-875")
@pytest.mark.parametrize("query", list(ROW_BESIDE_NON_ROW))
def test_a_row_beside_a_non_row_transforms_its_operands_first(query: str, dsn: str) -> None:
    assert answer(dsn, query) == ROW_BESIDE_NON_ROW[query]
