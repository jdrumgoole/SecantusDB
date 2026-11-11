"""What working RP-854 (a GROUP BY constant that is no integer) found:

- an ORDER BY constant that is no integer is not 42601;
- a DISTINCT ON constant that is no integer is not 42601;
- a GROUP BY position inside a grouping set or a parenthesised list is
  not read as a position;
- a grouped query reports a column that does not exist as 42803, not
  42703.

Measured on PostgreSQL 15.19 (the shared reference on port 5415): every
statement is VALUES-only and runs there as written.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"


def _answer(dsn: str, query: str) -> object:
    """The rows, or the error's SQLSTATE, first line and position."""
    with psycopg.connect(dsn, autocommit=True) as c:
        try:
            cur = c.execute(query)
            assert cur.description is not None
            return [tuple(r) for r in cur.fetchall()]
        except psycopg.Error as e:
            return ("error", e.sqlstate, str(e).splitlines()[0], e.diag.statement_position)


@pytest.mark.backlog("RP-871")
@pytest.mark.parametrize(
    ("query", "position"),
    [
        ("select 1 from (values (1)) v(a) order by 2.5", "42"),
        ("select a from (values (1)) v(a) group by a order by 'x'", "53"),
    ],
)
def test_a_non_integer_order_by_constant_is_42601(query: str, position: str, dsn: str) -> None:
    """`findTargetlistEntrySQL92` refuses an ORDER BY constant that is no
    integer as 42601 `non-integer constant in ORDER BY` at the constant on
    PostgreSQL 15.19. Here the first is 0A000 `ORDER BY over an expression
    is not supported yet`, and the second returns its row."""
    assert _answer(dsn, query) == (
        "error",
        "42601",
        "non-integer constant in ORDER BY",
        position,
    )


@pytest.mark.backlog("RP-872")
@pytest.mark.parametrize(
    "query",
    [
        "select distinct on (1.5) a from (values (1)) v(a)",
        "select distinct on ('x') a from (values (1)) v(a)",
    ],
)
def test_a_non_integer_distinct_on_constant_is_42601(query: str, dsn: str) -> None:
    """`findTargetlistEntrySQL92` refuses a DISTINCT ON constant that is no
    integer as 42601 `non-integer constant in DISTINCT ON` at the constant
    on PostgreSQL 15.19. Here each returns its row."""
    assert _answer(dsn, query) == (
        "error",
        "42601",
        "non-integer constant in DISTINCT ON",
        "21",
    )


@pytest.mark.backlog("RP-873")
@pytest.mark.parametrize(
    ("query", "answer"),
    [
        (
            "select a from (values (1), (2)) v(a) group by rollup(1) order by 1",
            [(1,), (2,), (None,)],
        ),
        (
            "select a from (values (1), (2)) v(a) group by grouping sets ((1), ()) order by 1",
            [(1,), (2,), (None,)],
        ),
        (
            "select a, a + 1 from (values (1), (2)) v(a) group by (1, 2) order by 1",
            [(1, 2), (2, 3)],
        ),
        (
            "select 1 from (values (1)) v(a) group by rollup(9)",
            ("error", "42P10", "GROUP BY position 9 is not in select list", "49"),
        ),
        (
            "select 1 from (values (1)) v(a) group by (9, 1.5)",
            ("error", "42P10", "GROUP BY position 9 is not in select list", "43"),
        ),
    ],
)
def test_a_group_by_position_inside_a_set_or_list_is_a_position(
    query: str, answer: object, dsn: str
) -> None:
    """`transformGroupClause` flattens grouping sets and a parenthesised
    list (`flatten_grouping_sets`) and reads each integer constant in them
    as a select-list position, as at the top level, on PostgreSQL 15.19.
    Here the constant is grouped as a value: a column the position names
    is 42803 `must appear in the GROUP BY clause`, and a position past the
    select list is 0A000 `this target is not supported yet` or the
    42601 of a later item."""
    assert _answer(dsn, query) == answer


@pytest.mark.backlog("RP-874")
@pytest.mark.parametrize(
    ("query", "position"),
    [
        ("select nosuch from (values (1)) v(a) group by a", "8"),
        ("select a, nosuch from (values (1)) v(a) group by a", "11"),
    ],
)
def test_an_undefined_column_in_a_grouped_query_is_42703(
    query: str, position: str, dsn: str
) -> None:
    """`transformColumnRef` fails on a name no FROM item has while the
    select list is analysed, before `parseCheckAggregates` looks at what
    is grouped: 42703 `column "nosuch" does not exist` on PostgreSQL 15.19.
    Here it is 42803 `column "nosuch" must appear in the GROUP BY clause or
    be used in an aggregate function`."""
    assert _answer(dsn, query) == (
        "error",
        "42703",
        'column "nosuch" does not exist',
        position,
    )
