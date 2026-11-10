"""What working RP-699 (a correlated subquery's pass-through of an outer
FROM function's column or a whole row's field keeps its type) found:

- a FROM function's whole row under a column alias is 42703 inside an
  expression (RP-702);
- a correlated subquery in the WHERE of a query over a FROM function was
  42883 and named the internal per-row call (RP-703, fixed);
- `(v).*` in a select list was 0A000 (RP-704, fixed);
- a field of an outer item's whole row in a LATERAL subquery is 42703
  (RP-705).

And what working RP-704 found: a star over a FROM subquery in a grouped
query is 42803 with no column name (RP-852).

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


# A Function Scan applies its WHERE to each row the function returns. Each
# of the first two was 42883 `function \x1fcorrelated(integer, unknown, ...)
# does not exist`, the call standing in for the subquery reaching the
# client, and the third 0A000 `AExpr is not supported yet`: a WHERE over a
# `generate_series` that did not lower to a filter was refused rather than
# run per row.
FUNCTION_SCAN_CORRELATED_WHERE = {
    "select 1 from generate_series(1, 2) f(x) where (select f.x + 1) > 0": (
        [(1,), (1,)],
        ["?column?"],
    ),
    "select 1 from generate_series(1, 2) f(x) where exists (select 1 where f.x > 1)": (
        [(1,)],
        ["?column?"],
    ),
    "select 1 from generate_series(1, 2) f(x) where f.x + 1 > 0": ([(1,), (1,)], ["?column?"]),
}


@pytest.mark.backlog_fixed("RP-703")
@pytest.mark.parametrize("query", list(FUNCTION_SCAN_CORRELATED_WHERE))
def test_correlated_subquery_in_a_function_scan_where(query: str, dsn: str) -> None:
    assert answer(dsn, query) == FUNCTION_SCAN_CORRELATED_WHERE[query]


# `transformIndirection` expands a whole row's star as `v.*`. Each over a
# single FROM item was 0A000 `this field selection is not supported yet`
# (a join's already expanded). A column of the name wins: `(v).*` of an
# integer column is 42809.
WHOLE_ROW_STAR = {
    "select (v).* from (values (1)) v(a)": ([(1,)], ["a"]),
    "select (v).* from (values (1, 'x')) v(a, b)": ([(1, "x")], ["a", "b"]),
    "select (s).* from (select 1 a, 2 b) s": ([(1, 2)], ["a", "b"]),
    "select (v).* from (values (1)) as v": ([(1,)], ["column1"]),
    "select (n).* from pg_namespace n where nspname = 'public'": (
        [
            (
                2200,
                "public",
                6171,
                ["pg_database_owner=UC/pg_database_owner", "=U/pg_database_owner"],
            )
        ],
        ["oid", "nspname", "nspowner", "nspacl"],
    ),
    "with c as (select 1 a, 'q' b) select (c).* from c": ([(1, "q")], ["a", "b"]),
    "select 0 as k, (v).*, 1 as j from (values (1)) v(a)": ([(0, 1, 1)], ["k", "a", "j"]),
    "select (v).* from (values (2), (1)) v(a) where (v).a > 0 order by 1": (
        [(1,), (2,)],
        ["a"],
    ),
    "select distinct (v).* from (values (1), (1)) v(a)": ([(1,)], ["a"]),
    "select (v).* from (values (1, 2)) v(a, b) where false": ([], ["a", "b"]),
    "select (v.*).* from (values (1)) v(a)": ([(1,)], ["a"]),
    "select (v).* from (values (1)) v(a) union all select 2": ([(1,), (2,)], ["a"]),
    "select (select (v).* from (values (1)) v(a))": ([(1,)], ["a"]),
    "select (v).* from (values (1)) v(v)": ("42809", "type integer is not composite", None),
    "select (f).* from generate_series(1, 2) f": (
        "42809",
        "type integer is not composite",
        None,
    ),
}


@pytest.mark.backlog_fixed("RP-704")
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


# A star over a FROM subquery in a grouped query is its columns, each
# grouped or functionally dependent as PostgreSQL checks it
# (`check_ungrouped_columns`). Here each is 42803 `column "" must appear in
# the GROUP BY clause ...` with no position: `expand_star_in_grouped`
# expands a star over a table only. Found while working RP-704.
GROUPED_SUBQUERY_STAR = {
    "select v.* from (values (1), (1)) v(a) group by a": ([(1,)], ["a"]),
    "select (v).*, count(*) from (values (1), (1)) v(a) group by v.a": (
        [(1, 2)],
        ["a", "count"],
    ),
    "select v.* from (values (1)) v(a) group by v": (
        "42803",
        'column "v.a" must appear in the GROUP BY clause or be used in an aggregate function',
        "8",
    ),
}


@pytest.mark.backlog("RP-852")
@pytest.mark.parametrize("query", list(GROUPED_SUBQUERY_STAR))
def test_a_star_over_a_from_subquery_in_a_grouped_query(query: str, dsn: str) -> None:
    assert answer(dsn, query) == GROUPED_SUBQUERY_STAR[query]
