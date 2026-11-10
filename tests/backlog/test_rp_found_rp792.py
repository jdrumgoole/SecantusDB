"""What working RP-792 (an IN list's members against a `char(n)` operand,
per row) found:

- an IN list's untyped member that the list's common type cannot read is
  no error once an earlier member matches, or when there is no row
  (RP-806).

Measured on the shared PostgreSQL 15.19 reference (port 5415), FROM-less
and VALUES-only statements.
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
    """The rows, or the error's SQLSTATE and first line."""
    with psycopg.connect(dsn, autocommit=True) as c:
        try:
            return [tuple(r) for r in c.execute(query).fetchall()]
        except psycopg.Error as e:
            return ("error", e.sqlstate, str(e).splitlines()[0])


_X = ("error", "22P02", 'invalid input syntax for type integer: "x"')


@pytest.mark.backlog("RP-806")
@pytest.mark.parametrize(
    "query",
    [
        "select 1 in (1, 'x')",
        "select x, x in (1, 'x') from (values (1)) s(x)",
        "select x, x in (1, 'x') from (values (1)) s(x) where false",
    ],
)
def test_an_in_lists_unreadable_member_is_an_error(dsn: str, query: str) -> None:
    """`transformAExprIn` coerces every non-Var member to the list's common
    type, `int4`, while the statement is analysed, so `'x'` is 22P02 on
    PostgreSQL 15.19 before any row is compared. Here the members are
    compared one at a time and the first match ends the list."""
    assert _answer(dsn, query) == _X
