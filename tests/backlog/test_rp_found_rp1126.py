"""RP-1126 (a plain DISTINCT whose ORDER BY is a prefix of it sorted on
the ORDER BY alone), and what working it found:

- the rows under the Unique came back in the order the DISTINCT kept
  them within each ORDER BY value, where PostgreSQL sorts them on every
  other DISTINCT column after the ORDER BY (fixed: RP-1126);
- an ORDER BY expression a DISTINCT does not list is refused at its
  operator, not at the expression's leftmost token (RP-1293).

Measured on PostgreSQL 15.19 (the shared reference on port 5415).
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    base = _dsn(rust_pg)
    with psycopg.connect(base + " dbname=postgres", autocommit=True) as c:
        c.execute("drop database if exists rp1126")
        c.execute("create database rp1126")
    with psycopg.connect(base + " dbname=rp1126", autocommit=True) as c:
        c.execute("create table e (a int, b text)")
        c.execute("insert into e select g, 'v' || (g % 10) from generate_series(1, 1000) g")
        c.execute("analyze e")
    yield base + " dbname=rp1126"


def _answer(dsn: str, query: str) -> object:
    """The rows, or the error's SQLSTATE and position."""
    with psycopg.connect(dsn, autocommit=True) as c:
        try:
            return [tuple(r) for r in c.execute(query).fetchall()]
        except psycopg.Error as e:
            return ("error", e.sqlstate, e.diag.statement_position)


def _pairs(*groups: tuple[str, list[int]]) -> list[tuple[str, int]]:
    return [(b, v) for b, vs in groups for v in vs]


@pytest.mark.backlog_fixed("RP-1126")
@pytest.mark.parametrize(
    ("query", "expected"),
    [
        (
            "select distinct b, a+1 from e where a < 25 order by b",
            _pairs(
                ("v0", [11, 21]),
                ("v1", [2, 12, 22]),
                ("v2", [3, 13, 23]),
                ("v3", [4, 14, 24]),
                ("v4", [5, 15, 25]),
                ("v5", [6, 16]),
                ("v6", [7, 17]),
                ("v7", [8, 18]),
                ("v8", [9, 19]),
                ("v9", [10, 20]),
            ),
        ),
        (
            "select distinct b, a from e where a < 25 order by b desc",
            _pairs(
                ("v9", [9, 19]),
                ("v8", [8, 18]),
                ("v7", [7, 17]),
                ("v6", [6, 16]),
                ("v5", [5, 15]),
                ("v4", [4, 14, 24]),
                ("v3", [3, 13, 23]),
                ("v2", [2, 12, 22]),
                ("v1", [1, 11, 21]),
                ("v0", [10, 20]),
            ),
        ),
    ],
)
def test_a_distinct_sorts_on_every_column_after_its_order_by(
    query: str, expected: list[tuple[str, int]], dsn: str
) -> None:
    """PostgreSQL 15.19 plans these as a Unique over a Sort on the ORDER
    BY then every other DISTINCT column, ascending
    (`transformDistinctClause`), so the rows come out in that order. Here
    they came back in the order the DISTINCT kept them within each `b`."""
    assert _answer(dsn, query) == expected


@pytest.mark.backlog("RP-1293")
@pytest.mark.parametrize(
    ("query", "position"),
    [
        ("select distinct a from (values (1)) v(a) order by a+0", "51"),
        ("select distinct a from (values (1)) v(a) order by a * 2 desc", "51"),
        ("select distinct a, b from (values (1, 2)) v(a, b) order by a, b - a", "63"),
    ],
)
def test_an_unlisted_order_by_expression_is_refused_at_its_start(
    query: str, position: str, dsn: str
) -> None:
    """PostgreSQL 15.19 refuses each as 42P10 at the expression's
    leftmost token: `exprLocation` of an `A_Expr` is the leftmost of its
    operator and its left operand. Here the position is the operator's."""
    assert _answer(dsn, query) == ("error", "42P10", position)
