"""What working RP-872 (a DISTINCT ON constant that is no integer) found:

- a DISTINCT ON position past the select list returns rows, and one over
  a star is refused;
- a window definition's error comes ahead of a GROUP BY or DISTINCT ON
  constant's.

Measured on PostgreSQL 15.19 (the shared reference on port 5415): every
statement is VALUES-only and runs there as written.
"""

from __future__ import annotations

from collections.abc import Iterator

import pytest

from tests.backlog.test_rp_batch26 import _dsn
from tests.backlog.test_rp_found_rp854 import _answer


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"


@pytest.mark.backlog("RP-884")
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
    ],
)
def test_a_distinct_on_position_is_checked_against_the_select_list(
    query: str, answer: object, dsn: str
) -> None:
    """`findTargetlistEntrySQL92` reads a DISTINCT ON integer as a position
    in the select list as expanded, and refuses one past its end as 42P10
    at the constant on PostgreSQL 15.19. Here a position past a plain list
    is a key of one value, and one over a star is 0A000."""
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
