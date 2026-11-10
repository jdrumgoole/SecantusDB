"""What working RP-707 (a `text` scalar subquery is typed `text`) found:

- a scalar subquery whose column is an untyped literal (`(select 'a')`)
  is still read as `unknown` by the expression around it, so it is
  coerced to the other side's type, where PostgreSQL resolves the
  subquery's column to `text` and finds no operator;
- a scalar subquery as an element of an `IN` list is refused.

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
    """The column type OIDs and rows, or the error's SQLSTATE and first
    line."""
    with psycopg.connect(dsn, autocommit=True) as c:
        try:
            cur = c.execute(query)
            assert cur.description is not None
            return [d.type_code for d in cur.description], [tuple(r) for r in cur.fetchall()]
        except psycopg.Error as e:
            return ("error", e.sqlstate, str(e).splitlines()[0])


@pytest.mark.backlog("RP-756")
@pytest.mark.parametrize(
    ("query", "expected"),
    [
        (
            "select (select 'a') = 1",
            ("error", "42883", "operator does not exist: text = integer"),
        ),
        (
            "select 1 = (select '1')",
            ("error", "42883", "operator does not exist: integer = text"),
        ),
        (
            "select (select '2020-01-01') < now()",
            ("error", "42883", "operator does not exist: text < timestamp with time zone"),
        ),
        (
            "select case when false then 1 else (select 'a') end",
            ("error", "42804", "CASE types text and integer cannot be matched"),
        ),
    ],
)
def test_an_untyped_literal_subquery_is_text_outside_it(
    query: str, expected: object, dsn: str
) -> None:
    """PostgreSQL 15.19 resolves an untyped literal in a subquery's target
    list to `text` (`resolveTargetListUnknowns`), so the subquery is a
    `text` value to the expression around it. Here the outer expression
    still coerces it as `unknown`: `1 = (select '1')` and `(select 'a') =
    1` are 0A000 `comparing ... is not supported yet`, the `< now()`
    answers true, and a CASE casts it to `integer` (22P02)."""
    assert _answer(dsn, query) == expected


@pytest.mark.backlog("RP-757")
def test_a_scalar_subquery_in_an_in_list(dsn: str) -> None:
    """A scalar subquery as one element of an `IN` list is an ordinary
    expression on PostgreSQL 15.19 (`x = sub OR x = 'tt'`); here the
    statement is 0A000 `SubLink is not supported yet`."""
    assert _answer(
        dsn,
        "select x from (values ('tt'::text), ('uu'), ('vv')) s(x)"
        " where x in ((select max(y) from (values ('uu'::text)) t(y)), 'tt') order by x",
    ) == ([25], [("tt",), ("uu",)])
