"""What working RP-756 (an untyped-literal subquery is `text` outside it)
found:

- a FROM subquery that is a set operation is not typed by the operator
  check, so a comparison of its text column with an integer is 0A000 at
  run time where PostgreSQL is 42883 as the statement is analysed.

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


@pytest.mark.backlog("RP-765")
@pytest.mark.parametrize(
    "query",
    [
        "select x = 1 from (select 'a'::text union select 'b') s(x)",
        "select x = 1 from (select 'a' union select 'b') s(x)",
    ],
)
def test_a_set_operation_from_subquerys_column_is_typed(query: str, dsn: str) -> None:
    """A FROM subquery's set operation has the arms' common column type
    (`transformSetOperationTree`; `text` when every arm is an untyped
    literal), so `x = 1` over its text column is 42883 `operator does not
    exist: text = integer` at 10 on PostgreSQL 15.19. Here the operator
    check does not type a set operation's columns (`add_derived` gives
    up), and the comparison is 0A000 `comparing string with int32 using =
    is not supported yet`, with no position."""
    assert _answer(dsn, query) == (
        "error",
        "42883",
        "operator does not exist: text = integer",
        "10",
    )
