"""What working RP-700 (a GROUP BY naming a correlated subquery target)
found:

- a grouped query with a subquery in its select list reports a column
  that is neither grouped nor aggregated as 42703 `column "b" does not
  exist`, where PostgreSQL is 42803 `must appear in the GROUP BY clause`;
- a GROUP BY constant that is no integer was not 42601 (fixed: RP-854);
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


@pytest.mark.backlog_fixed("RP-854")
@pytest.mark.parametrize(
    ("query", "position"),
    [
        ("select 1 from (values (1)) v(a) group by 1.5", "42"),
        ("select 1 from (values (1)) v(a) group by 'x'", "42"),
        ("select 1 from (values (1)) v(a) group by null", "42"),
        ("select 1 from (values (1)) v(a) group by true", "42"),
        ("select (select v.a) from (values (1)) v(a) group by 1.5", "53"),
        ("select 1 from (values (1)) v(a) group by (1.5)", "43"),
        ("select 1 from (values (1)) v(a) group by -1.5", "42"),
        ("select 1 from (values (1)) v(a) group by 2147483648", "42"),
        ("select 1 from (values (1)) v(a) group by B'101'", "42"),
        ("select 1 from (values (1)) v(a) group by a, 'x'", "45"),
        ("select 1 from (values (1)) v(a) group by (1.5, 2)", "43"),
        ("select 1 from (values (1)) v(a) group by rollup(1.5, 9)", "49"),
        ("select 1 from (values (1)) v(a) group by grouping sets ((), ('x'))", "62"),
        ("select 1 from (values (1)) v(a) group by distinct 1.5", "51"),
        ("select 1 group by 'x'", "19"),
        ("select a from (values (1)) v(a) group by 1.5", "42"),
        ("select count(*) from (values (1)) v(a) group by 1.5", "49"),
        ("select grouping(a) from (values (1)) v(a) group by 1.5", "52"),
        ("select 1/0 from (values (1)) v(a) group by 1.5", "44"),
        ("select 1 from (values (1)) v(a) group by 1.5 for update", "42"),
        ("select * from (select 1 from (values (1)) v(a) group by 'x') s", "57"),
        ("select (select 1 from (values (1)) w(b) group by 'x') from (values (1)) v(a)", "50"),
        ("with c as (select 1 from (values (1)) v(a) group by 1.5) select * from c", "53"),
        ("select 1 from (values (1)) v(a) group by 1.5 union select 2", "42"),
        ("explain select 1 from (values (1)) v(a) group by 1.5", "50"),
    ],
)
def test_a_non_integer_group_by_constant_is_42601(query: str, position: str, dsn: str) -> None:
    """`findTargetlistEntrySQL92` refuses any GROUP BY constant that is no
    integer -- a numeric, a string, NULL, a boolean -- as 42601 `non-integer
    constant in GROUP BY` at the constant on PostgreSQL 15.19, ahead of the
    ungrouped-column and GROUPING() checks, the locking clause and anything
    a plan or a run raises. Each was 0A000 `this target is not supported
    yet`, with no position, a 42803, or rows."""
    assert _answer(dsn, query) == (
        "error",
        "42601",
        "non-integer constant in GROUP BY",
        position,
    )


@pytest.mark.backlog_fixed("RP-854")
@pytest.mark.parametrize(
    ("query", "error"),
    [
        (
            "select nosuch from (values (1)) v(a) group by 1.5",
            ("42703", 'column "nosuch" does not exist', "8"),
        ),
        (
            "select a from (values (1)) v(a) where nosuch group by 1.5",
            ("42703", 'column "nosuch" does not exist', "39"),
        ),
        (
            "select a from (values (1)) v(a) group by 1.5 having nosuch",
            ("42703", 'column "nosuch" does not exist', "53"),
        ),
        (
            "select 1 from (values (1)) v(a) group by 1.5 order by nosuch",
            ("42703", 'column "nosuch" does not exist', "55"),
        ),
        (
            "select 1 from (values (1)) v(a) group by 1.5 order by 7",
            ("42P10", "ORDER BY position 7 is not in select list", "55"),
        ),
        (
            "select 1 from (values (1)) v(a) where count(*) > 0 group by 1.5",
            ("42803", "aggregate functions are not allowed in WHERE", "39"),
        ),
        (
            "select 1 from (values (1)) v(a) group by 9, 1.5",
            ("42P10", "GROUP BY position 9 is not in select list", "42"),
        ),
    ],
)
def test_what_parse_analysis_meets_before_the_group_by_constant_comes_first(
    query: str, error: tuple[str, str, str], dsn: str
) -> None:
    """`transformGroupClause` runs after the select list, WHERE, HAVING and
    ORDER BY are analysed, and takes its items in order, so their errors
    come before a non-integer GROUP BY constant's on PostgreSQL 15.19."""
    assert _answer(dsn, query) == ("error", *error)


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
