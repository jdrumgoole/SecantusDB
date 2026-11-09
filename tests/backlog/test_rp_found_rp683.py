"""What working RP-683 (a correlated `ARRAY(...)` over a `char(n)` column
keeps its elements' blanks) found:

- a correlated `ARRAY(...)` over an array column was typed one dimension too
  many, and reached the client as text (RP-729, fixed: the regression test
  below).

Measured on PostgreSQL 15.19 (the shared reference on port 5415): every
statement is VALUES-only and runs there as written.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

W = "(values (1, array[1, null]), (2, array[2, 3]))"


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"


def _answer(dsn: str, query: str) -> object:
    """Each column's type OID and the rows, or the error's SQLSTATE and first line."""
    with psycopg.connect(dsn, autocommit=True) as c:
        try:
            cur = c.execute(query)
            types = [cur.pgresult.ftype(i) for i in range(cur.pgresult.nfields)]
            return types, cur.fetchall()
        except psycopg.Error as e:
            return ("error", e.sqlstate, str(e).splitlines()[0])


@pytest.mark.backlog_fixed("RP-729")
def test_correlated_array_of_an_array_column_is_its_own_type(dsn: str) -> None:
    sub = f"array(select t.f from {W} t(k, f) where t.k >= s.k)"
    assert _answer(dsn, f"select k, {sub} from {W} s(k, f) order by k") == (
        [23, 1007],
        [(1, [[1, None], [2, 3]]), (2, [[2, 3]])],
    )
    assert _answer(dsn, f"select k, pg_typeof({sub})::text from {W} s(k, f) order by k") == (
        [23, 25],
        [(1, "integer[]"), (2, "integer[]")],
    )
