"""What working RP-699 (a correlated subquery's pass-through of an outer
FROM function's column or a whole row's field keeps its type) found:

- a FROM function's whole row under a column alias is 42703 inside an
  expression (RP-702);
- a correlated subquery in the WHERE of a query over a FROM function is
  42883 and names the internal per-row call (RP-703);
- `(v).*` in a select list is 0A000 (RP-704);
- a field of an outer item's whole row in a LATERAL subquery is 42703
  (RP-705).

An answer is the rows and the column names, or `(sqlstate, message,
position)` for an error. Measured on PostgreSQL 15.19; none needs a table.
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
            return (e.sqlstate, e.diag.message_primary, e.diag.statement_position)
        return (cur.fetchall(), [d.name for d in cur.description or []])


# The whole row of a function returning a base type is that value, whatever
# its column is called; `(f).x` of it is 42809. Here each is 42703 `column
# "f" does not exist`, though a bare `select f` answers.
FUNCTION_WHOLE_ROW_ALIASED = {
    "select f + 1 from generate_series(1, 2) f(x)": ([(2,), (3,)], ["?column?"]),
    "select coalesce(f) from generate_series(1, 2) f(x)": ([(1,), (2,)], ["coalesce"]),
    "select (select f) from generate_series(1, 2) f(x)": ([(1,), (2,)], ["f"]),
    "select (select (f).x) from generate_series(1, 2) f(x)": (
        "42809",
        "column notation .x applied to type integer, which is not a composite type",
        "17",
    ),
}


@pytest.mark.backlog("RP-702")
@pytest.mark.parametrize("query", list(FUNCTION_WHOLE_ROW_ALIASED))
def test_function_whole_row_under_a_column_alias(query: str, dsn: str) -> None:
    assert answer(dsn, query) == FUNCTION_WHOLE_ROW_ALIASED[query]


# Here each is 42883 `function \x1fcorrelated(integer, unknown, ...) does
# not exist`: the call standing in for the subquery reaches the client.
FUNCTION_SCAN_CORRELATED_WHERE = {
    "select 1 from generate_series(1, 2) f(x) where (select f.x + 1) > 0": (
        [(1,), (1,)],
        ["?column?"],
    ),
    "select 1 from generate_series(1, 2) f(x) where exists (select 1 where f.x > 1)": (
        [(1,)],
        ["?column?"],
    ),
}


@pytest.mark.backlog("RP-703")
@pytest.mark.parametrize("query", list(FUNCTION_SCAN_CORRELATED_WHERE))
def test_correlated_subquery_in_a_function_scan_where(query: str, dsn: str) -> None:
    assert answer(dsn, query) == FUNCTION_SCAN_CORRELATED_WHERE[query]


# `transformIndirection` expands a whole row's star as `v.*`. Here each is
# 0A000 `this field selection is not supported yet`.
WHOLE_ROW_STAR = {
    "select (v).* from (values (1)) v(a)": ([(1,)], ["a"]),
    "select (v).* from (values (1, 'x')) v(a, b)": ([(1, "x")], ["a", "b"]),
}


@pytest.mark.backlog("RP-704")
@pytest.mark.parametrize("query", list(WHOLE_ROW_STAR))
def test_whole_row_star_in_a_select_list(query: str, dsn: str) -> None:
    assert answer(dsn, query) == WHOLE_ROW_STAR[query]


# A LATERAL subquery reads the earlier item's whole row as an expression
# subquery does. Here each is 42703 `column "v" does not exist` at the `v`.
LATERAL_WHOLE_ROW_FIELD = {
    "select l.* from (values (3)) v(a), lateral (select (v).a + 0 as k) l": ([(3,)], ["k"]),
    "select l.k from (values (3)) v(a), lateral (select 1 as k where (v).a = 3) l": (
        [(1,)],
        ["k"],
    ),
}


@pytest.mark.backlog("RP-705")
@pytest.mark.parametrize("query", list(LATERAL_WHOLE_ROW_FIELD))
def test_lateral_whole_row_field(query: str, dsn: str) -> None:
    assert answer(dsn, query) == LATERAL_WHOLE_ROW_FIELD[query]
