"""What working RP-854 (a GROUP BY constant that is no integer) found:

- an ORDER BY constant that is no integer is not 42601;
- a DISTINCT ON constant that is no integer was not 42601 (fixed: RP-872);
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


@pytest.mark.backlog_fixed("RP-872")
@pytest.mark.parametrize(
    ("query", "position"),
    [
        ("select distinct on (1.5) a from (values (1)) v(a)", "21"),
        ("select distinct on ('x') a from (values (1)) v(a)", "21"),
        ("select distinct on (-1.5) a from (values (1)) v(a)", "21"),
        ("select distinct on (null) a from (values (1)) v(a)", "21"),
        ("select distinct on (true) a from (values (1)) v(a)", "21"),
        ("select distinct on (b'1') a from (values (1)) v(a)", "21"),
        ("select distinct on ((1.5)) a from (values (1)) v(a)", "22"),
        ("select distinct on (1.5, 9) a from (values (1)) v(a)", "21"),
        ("select distinct on (b, 1.5) a, b from (values (1, 2)) v(a, b) order by a", "24"),
        ("select distinct on (1.5) a from (values (1)) v(a) order by a", "21"),
        ("select distinct on (1.5) a, b from (values (1, 2)) v(a, b) group by a", "21"),
        ("select distinct on (1.5) a from (values (1)) v(a) for update", "21"),
        ("select distinct on (1.5) a from (values (1)) v(a) limit 'x'", "21"),
        ("select distinct on (1.5) a from (values (1)) v(a) limit a", "21"),
        ("select distinct on (1.5) a from (values (1)) v(a) offset a", "21"),
        ("select distinct on (1.5) sum(a) from (values (1)) v(a)", "21"),
        ("select * from (select distinct on (1.5) a from (values (1)) v(a)) s", "36"),
        ("with q as (select distinct on (1.5) a from (values (1)) v(a)) select * from q", "32"),
        ("select (select distinct on (1.5) a from (values (1)) v(a))", "29"),
        ("select distinct on (1.5) a from (values (1)) v(a) union select 1", "21"),
    ],
)
def test_a_non_integer_distinct_on_constant_is_42601(query: str, position: str, dsn: str) -> None:
    """`findTargetlistEntrySQL92` refuses a DISTINCT ON constant that is no
    integer as 42601 `non-integer constant in DISTINCT ON` at the constant
    on PostgreSQL 15.19, ahead of the ORDER BY match, the ungrouped-column
    check, LIMIT, OFFSET and the locking clause. Each returned its row, or
    a later clause's error."""
    assert _answer(dsn, query) == (
        "error",
        "42601",
        "non-integer constant in DISTINCT ON",
        position,
    )


@pytest.mark.backlog_fixed("RP-872")
@pytest.mark.parametrize(
    ("query", "answer"),
    [
        (
            "select distinct on (nosuch, 1.5) a from (values (1)) v(a)",
            ("error", "42703", 'column "nosuch" does not exist', "21"),
        ),
        (
            "select distinct on (1.5) a from (values (1)) v(a) group by 2.5",
            ("error", "42601", "non-integer constant in GROUP BY", "60"),
        ),
        (
            "select distinct on (a) a from (values (1)) v(a) group by 1.5",
            ("error", "42601", "non-integer constant in GROUP BY", "58"),
        ),
        (
            "select distinct on (1.5) a from (values (1)) v(a) order by nosuch",
            ("error", "42703", 'column "nosuch" does not exist', "60"),
        ),
        (
            "select distinct on (1.5) a from (values (1)) v(a) where nosuch",
            ("error", "42703", 'column "nosuch" does not exist', "57"),
        ),
        ("select distinct on ('x'::text) a from (values (1)) v(a)", [(1,)]),
        ("select distinct on (1 + 0.5) a from (values (1)) v(a)", [(1,)]),
        ("select distinct on (1) a from (values (1)) v(a)", [(1,)]),
    ],
)
def test_what_comes_before_a_distinct_on_constant(query: str, answer: object, dsn: str) -> None:
    """On PostgreSQL 15.19 an earlier DISTINCT ON item, the GROUP BY, the
    ORDER BY, WHERE and the select list are analysed before a DISTINCT ON
    constant, so their errors come first; a constant under a cast or an
    operator is an expression and a key, and an integer is a position."""
    assert _answer(dsn, query) == answer


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
