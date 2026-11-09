"""What working RP-729 (a correlated `ARRAY(...)` over an array column is
its own type) found:

- an ARRAY sublink accumulates arrays of different shapes, or a NULL one,
  where PostgreSQL refuses each (RP-730).

Measured on PostgreSQL 15.19 (the shared reference on port 5415): every
statement is VALUES-only and runs there as written.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

DIMS = ("2202E", "cannot accumulate arrays of different dimensionality")
NULLS = ("22004", "cannot accumulate null arrays")


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"


def _answer(dsn: str, query: str) -> object:
    """The rows, or the error's SQLSTATE and first line."""
    with psycopg.connect(dsn, autocommit=True) as c:
        try:
            return c.execute(query).fetchall()
        except psycopg.Error as e:
            return (e.sqlstate, str(e).splitlines()[0])


@pytest.mark.backlog("RP-730")
def test_array_sublink_refuses_ragged_or_null_element_arrays(dsn: str) -> None:
    """`accumArrayResultArr` (PostgreSQL 15.19): every element array has the
    first one's dimensions and lengths, and none is NULL."""
    for rows, want in (
        ("(array[1,2]), (array[3])", DIMS),
        ("(array[1]), (array[[3]])", DIMS),
        ("(array[1]), ('{}'::int[])", DIMS),
        ("(array[1]), (null::int[])", NULLS),
    ):
        assert _answer(dsn, f"select array(select f from (values {rows}) t(f))") == want, rows
    correlated = (
        "select k, array(select t.f from (values (1, array[1,2]), (2, array[3])) t(k, f)"
        " where t.k >= s.k) from (values (1)) s(k)"
    )
    assert _answer(dsn, correlated) == DIMS
