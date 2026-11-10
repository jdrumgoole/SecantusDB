"""What working RP-732 (a query with ORDER BY computes its select list
below the Sort) found:

- a DECLARE CURSOR computes every row's select list at the DECLARE, so a
  row no FETCH reaches raises its error there;
- a sorted cursor computes an expensive column below its Sort, where
  PostgreSQL's `cursor_tuple_fraction` postpones it;
- a cast over a CASE of untyped literals reports a branch's input error
  at the literal's position.

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


def _fetch_one(dsn: str, declare: str) -> object:
    """The first row a cursor declared by `declare` fetches, or the
    error's SQLSTATE and first line."""
    with psycopg.connect(dsn) as c:
        try:
            c.execute(declare)
            return c.execute("fetch 1 from c").fetchall()
        except psycopg.Error as e:
            return (e.sqlstate, str(e).splitlines()[0])


@pytest.mark.backlog("RP-744")
def test_a_cursor_computes_its_rows_as_they_are_fetched(dsn: str) -> None:
    """PostgreSQL 15.19 runs a cursor's plan as it is fetched: `fetch 1`
    computes the first row only, so the second row's division by zero is
    never raised. Here DECLARE computes every row and raises it."""
    assert _fetch_one(dsn, "declare c cursor for select 1/(k - 1) from (values (2), (1)) s(k)") == [
        (1,)
    ]


@pytest.mark.backlog("RP-745")
def test_a_sorted_cursor_postpones_an_expensive_column(dsn: str) -> None:
    """A cursor is planned with `cursor_tuple_fraction` (0.1), which
    `make_sort_input_target` treats as a LIMIT: an expensive column
    (`to_tsvector`, `procost` 100) is postponed past the Sort, so `fetch
    1` computes it for the row that sorts first only. PostgreSQL 15.19
    answers that row; here the column is computed below the Sort for every
    row, and the first row's missing configuration is raised."""
    assert _fetch_one(
        dsn,
        "declare c cursor for select k, to_tsvector(case when k = 1 then 'nope'"
        " else 'simple' end::regconfig, 'a')::text from (values (1), (2)) s(k)"
        " order by k desc",
    ) == [(2, "'a':1")]


@pytest.mark.backlog("RP-746")
def test_a_cast_over_a_case_of_literals_reports_no_position(dsn: str) -> None:
    """PostgreSQL 15.19 resolves a CASE of untyped literals to `text` and
    casts its result as the row runs, so the input error has no position.
    Here the cast is applied to each branch's literal and the error points
    at it."""
    with psycopg.connect(dsn, autocommit=True) as c, pytest.raises(psycopg.Error) as info:
        c.execute("select (case when k = 1 then 'x' else '5' end)::int from (values (1)) s(k)")
    e = info.value
    assert (e.sqlstate, str(e).splitlines()[0], e.diag.statement_position) == (
        "22P02",
        'invalid input syntax for type integer: "x"',
        None,
    )
