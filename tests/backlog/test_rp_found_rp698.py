"""What working RP-698 (a correlated set operation) found:

- an outer column in a set operation's ORDER BY is 42703 `column "b" does
  not exist`, where PostgreSQL's `transformSetOperationStmt` resolves it
  to the outer row and refuses it as 0A000 `invalid UNION/INTERSECT/EXCEPT
  ORDER BY clause` (RP-747).

Measured on PostgreSQL 15.19 (the shared reference on port 5415): every
statement is VALUES-only and runs there as written.
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


def answer(dsn: str, query: str) -> Any:
    with psycopg.connect(dsn, autocommit=True) as c:
        try:
            cur = c.execute(query)
        except psycopg.Error as e:
            return (
                e.sqlstate,
                e.diag.message_primary,
                e.diag.message_detail,
                e.diag.statement_position,
            )
        return cur.fetchall()


_ORDER_BY = "invalid UNION/INTERSECT/EXCEPT ORDER BY clause"
_ONLY_NAMES = "Only result column names can be used, not expressions or functions."

# The ORDER BY of a set operation sees only its output columns; a name the
# outer query has resolves to the outer row, which is a sort expression,
# not a result column. A name neither has is still 42703.
SET_OPERATION_ORDER_BY_OUTER = {
    "select (select a from (values (1)) t(a) union select 9 order by b limit 1)"
    " from (values (5, 6)) v(a, b)": ("0A000", _ORDER_BY, _ONLY_NAMES, "65"),
    "select (select a from (values (1)) t(a) union select 9 order by v.b limit 1)"
    " from (values (5, 6)) v(a, b)": ("0A000", _ORDER_BY, _ONLY_NAMES, "65"),
    "select (select 9 union select 8 order by b limit 1) from (values (5, 6)) v(a, b)": (
        "0A000",
        _ORDER_BY,
        _ONLY_NAMES,
        "42",
    ),
    "select (select a from (values (1)) t(a) union select 9 order by c limit 1)"
    " from (values (5, 6)) v(a, b)": ("42703", 'column "c" does not exist', None, "65"),
}


@pytest.mark.backlog("RP-747")
@pytest.mark.parametrize("query", list(SET_OPERATION_ORDER_BY_OUTER))
def test_an_outer_column_in_a_set_operations_order_by(query: str, dsn: str) -> None:
    assert answer(dsn, query) == SET_OPERATION_ORDER_BY_OUTER[query]
