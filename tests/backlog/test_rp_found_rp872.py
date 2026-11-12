"""What working RP-872 (a DISTINCT ON constant that is no integer) found:

- a DISTINCT ON position past the select list returned rows, and one over
  a star was refused (RP-884, fixed);
- a window definition's error comes ahead of a GROUP BY or DISTINCT ON
  constant's.

Measured on PostgreSQL 15.19 (the shared reference on port 5415): every
statement is VALUES-only and runs there as written.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn
from tests.backlog.test_rp_found_rp854 import _answer


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"


@pytest.mark.backlog_fixed("RP-884")
@pytest.mark.parametrize(
    ("query", "answer"),
    [
        (
            "select distinct on (9) a from (values (1)) v(a)",
            ("error", "42P10", "DISTINCT ON position 9 is not in select list", "21"),
        ),
        (
            "select distinct on (9, 1.5) a from (values (1)) v(a)",
            ("error", "42P10", "DISTINCT ON position 9 is not in select list", "21"),
        ),
        ("select distinct on (2) * from (values (1, 2)) v(a, b)", [(1, 2)]),
        (
            "select distinct on (3) * from (values (1, 2)) v(a, b)",
            ("error", "42P10", "DISTINCT ON position 3 is not in select list", "21"),
        ),
        (
            "select distinct on (0) a from (values (1)) v(a)",
            ("error", "42P10", "DISTINCT ON position 0 is not in select list", "21"),
        ),
        (
            "select distinct on (1, 9) a from (values (1)) v(a) order by nosuch",
            ("error", "42703", 'column "nosuch" does not exist', "61"),
        ),
        (
            "select distinct on (2) * from (values (1, 2), (3, 2)) v(a, b) order by 2, 1 desc",
            [(3, 2)],
        ),
        (
            "select distinct on (2) * from (values (1, 2), (3, 2)) v(a, b) order by 1",
            (
                "error",
                "42P10",
                "SELECT DISTINCT ON expressions must match initial ORDER BY expressions",
                "21",
            ),
        ),
    ],
)
def test_a_distinct_on_position_is_checked_against_the_select_list(
    query: str, answer: object, dsn: str
) -> None:
    """`findTargetlistEntrySQL92` reads a DISTINCT ON integer as a position
    in the select list as expanded, and refuses one outside it as 42P10 at
    the constant on PostgreSQL 15.19. A position past a plain list was a
    key of one value here, and one over a star was 0A000."""
    assert _answer(dsn, query) == answer


@pytest.mark.backlog("RP-885")
@pytest.mark.parametrize(
    ("query", "answer"),
    [
        (
            "select count(*) over w from (values (1)) v(a) group by 2.5"
            " window w as (order by nosuch)",
            ("error", "42601", "non-integer constant in GROUP BY", "56"),
        ),
        (
            "select distinct on (1.5) count(*) over w from (values (1)) v(a)"
            " window w as (order by nosuch)",
            ("error", "42601", "non-integer constant in DISTINCT ON", "21"),
        ),
    ],
)
def test_a_window_definition_is_analysed_after_the_constant(
    query: str, answer: object, dsn: str
) -> None:
    """`transformSelectStmt` runs `transformWindowDefinitions` after the
    GROUP BY and DISTINCT ON clauses on PostgreSQL 15.19, so a constant's
    42601 comes first. Here the window's 42703 does."""
    assert _answer(dsn, query) == answer


@pytest.mark.backlog_fixed("RP-884")
def test_a_values_column_of_a_domain_is_of_the_domain(dsn: str) -> None:
    """A VALUES list's column whose every row is of one domain is of that
    domain (`select_common_type`), so `CREATE TABLE AS VALUES` makes a
    column of the domain and enforces its CHECK, as on a private PostgreSQL
    15.19 cluster. RP-884 made a VALUES list's columns known to the star
    expansion; before, they were not, and the column was `integer`."""
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute("create domain d884 as int check (value > 0)")
        try:
            got = []
            for i, q in enumerate(
                [
                    "values (1::d884)",
                    "values (1::d884), (2)",
                    "select * from (values (1::d884, 'x')) v(a, b)",
                ]
            ):
                c.execute(f"create table c884_{i} as {q}")
                got.append(
                    c.execute(
                        "select format_type(atttypid, atttypmod) from pg_attribute"
                        f" where attrelid = 'c884_{i}'::regclass and attnum > 0"
                        " order by attnum"
                    ).fetchall()
                )
            assert got == [[("d884",)], [("integer",)], [("d884",), ("text",)]]
            with pytest.raises(psycopg.errors.CheckViolation):
                c.execute("insert into c884_0 select -1")
        finally:
            c.execute("drop table if exists c884_0, c884_1, c884_2")
            c.execute("drop domain d884")
