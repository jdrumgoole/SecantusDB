"""What working RP-700 (a GROUP BY naming a correlated subquery target)
found:

- a grouped query with a subquery in its select list reports a column
  that is neither grouped nor aggregated as 42703 `column "b" does not
  exist`, where PostgreSQL is 42803 `must appear in the GROUP BY clause`;
- a GROUP BY constant that is no integer is not 42601;
- a prepared LATERAL subquery holding a correlated subquery failed at its
  Describe (fixed in the same change: RP-856).

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
    """The column type OIDs and rows, or the error's SQLSTATE, first line
    and position."""
    with psycopg.connect(dsn, autocommit=True) as c:
        try:
            cur = c.execute(query)
            assert cur.description is not None
            return [d.type_code for d in cur.description], [tuple(r) for r in cur.fetchall()]
        except psycopg.Error as e:
            return ("error", e.sqlstate, str(e).splitlines()[0], e.diag.statement_position)


_UNGROUPED = 'column "v.b" must appear in the GROUP BY clause or be used in an aggregate function'


@pytest.mark.backlog("RP-853")
@pytest.mark.parametrize(
    ("query", "position"),
    [
        ("select b, (select 1) from (values (1, 2)) v(a, b) group by a", "8"),
        ("select (select v.a), b from (values (1, 2)) v(a, b) group by 1", "22"),
        ("select b, (select 1) from (values (1, 2)) v(a, b) group by a order by 1", "8"),
    ],
)
def test_an_ungrouped_column_beside_a_subquery_is_42803(
    query: str, position: str, dsn: str
) -> None:
    """`check_ungrouped_columns` reports `b`, neither grouped nor inside an
    aggregate, as 42803 naming it `v.b`, at the column, on PostgreSQL
    15.19. Here a grouped query with a subquery in its select list is split
    (`agg_hoist`) and `b` is read from the grouped FROM subquery, which
    has no such column: 42703 `column "b" does not exist`, with a HINT
    that `v` has one."""
    assert _answer(dsn, query) == ("error", "42803", _UNGROUPED, position)


@pytest.mark.backlog("RP-854")
@pytest.mark.parametrize(
    ("query", "position"),
    [
        ("select 1 from (values (1)) v(a) group by 1.5", "42"),
        ("select 1 from (values (1)) v(a) group by 'x'", "42"),
        ("select 1 from (values (1)) v(a) group by null", "42"),
        ("select 1 from (values (1)) v(a) group by true", "42"),
        ("select (select v.a) from (values (1)) v(a) group by 1.5", "53"),
    ],
)
def test_a_non_integer_group_by_constant_is_42601(query: str, position: str, dsn: str) -> None:
    """`findTargetlistEntrySQL92` refuses any GROUP BY constant that is no
    integer -- a numeric, a string, NULL, a boolean -- as 42601 `non-integer
    constant in GROUP BY` at the constant on PostgreSQL 15.19. Here each is
    0A000 `this target is not supported yet`, with no position."""
    assert _answer(dsn, query) == (
        "error",
        "42601",
        "non-integer constant in GROUP BY",
        position,
    )


def _prepared(dsn: str, query: str) -> object:
    """`_answer` through a named prepared statement (Parse, Describe, then
    Bind and Execute)."""
    with psycopg.connect(dsn, autocommit=True) as c:
        try:
            cur = c.execute(query, prepare=True)
            assert cur.description is not None
            return [d.type_code for d in cur.description], [tuple(r) for r in cur.fetchall()]
        except psycopg.Error as e:
            return ("error", e.sqlstate, str(e).splitlines()[0], e.diag.statement_position)


@pytest.mark.backlog_fixed("RP-856")
@pytest.mark.parametrize(
    ("query", "rows"),
    [
        (
            "select l.k from (values (1), (2)) v(a), lateral (select (select v.a) as k) l "
            "order by 1",
            [(1,), (2,)],
        ),
        (
            "select l.k from (values (1), (2)) v(a) left join lateral "
            "(select (select v.a + 1) as k) l on true order by 1",
            [(2,), (3,)],
        ),
        (
            "select (select v.a) from (values (1), (1), (2)) v(a) group by 1 order by 1",
            [(1,), (2,)],
        ),
    ],
)
def test_a_prepared_lateral_subquery_reads_a_correlated_subquery(
    query: str, rows: list[tuple[int]], dsn: str
) -> None:
    """A LATERAL subquery whose select list holds a scalar subquery over the
    left side's row prepares and runs on PostgreSQL 15.19, an `int4` column.
    Here a named prepared statement's Describe planned the LATERAL body
    over sample values with its correlated subquery RUN, and with no
    executor installed that was 0A000 `a correlated subquery evaluated
    outside a statement's execution is not supported yet`. The unnamed
    statement answered."""
    assert _prepared(dsn, query) == ([23], rows)
