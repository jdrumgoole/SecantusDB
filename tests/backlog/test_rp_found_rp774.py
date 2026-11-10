"""What working RP-774 (a scalar subquery under `IS TRUE` or `COLLATE`)
found:

- `IS [NOT] TRUE / FALSE / UNKNOWN` over a value that is not `boolean` was
  no error (RP-808, fixed).

Measured on PostgreSQL 15.19 (the shared reference on port 5415): every
statement is FROM-less or VALUES-only and creates nothing.
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


def _not_boolean(test: str, typ: str) -> tuple[str, str, str]:
    return ("error", "42804", f"argument of {test} must be type boolean, not type {typ}")


@pytest.mark.backlog_fixed("RP-808")
@pytest.mark.parametrize(
    ("query", "expected"),
    [
        ("select 1 is true", _not_boolean("IS TRUE", "integer")),
        ("select 1 is not false", _not_boolean("IS NOT FALSE", "integer")),
        ("select 0 is not true", _not_boolean("IS NOT TRUE", "integer")),
        ("select 1.5 is unknown", _not_boolean("IS UNKNOWN", "numeric")),
        ("select null::int is unknown", _not_boolean("IS UNKNOWN", "integer")),
        ("select 'x'::text is unknown", _not_boolean("IS UNKNOWN", "text")),
        ("select '1'::text is not unknown", _not_boolean("IS NOT UNKNOWN", "text")),
        ("select x is false from (values (1)) s(x)", _not_boolean("IS FALSE", "integer")),
        (
            "select x from (values (1), (0)) s(x) where x is true",
            _not_boolean("IS TRUE", "integer"),
        ),
        (
            "select x from (values (1), (0)) s(x) where x is not true",
            _not_boolean("IS NOT TRUE", "integer"),
        ),
        ("select (select 1) is true", _not_boolean("IS TRUE", "integer")),
        # An untyped literal is read as a boolean, as PostgreSQL does.
        ("select 'a' is true", ("error", "22P02", 'invalid input syntax for type boolean: "a"')),
        ("select 'yes' is true", [(True,)]),
    ],
)
def test_a_boolean_test_over_a_non_boolean_is_42804(dsn: str, query: str, expected: object) -> None:
    """`transformBooleanTest` coerced its argument to `boolean` and named
    the test when it could not: `select 1 is true` is 42804 on PostgreSQL
    15.19. Before RP-808 it was `false`, `null::int is unknown` was `true`,
    and `IS NOT TRUE` / `IS FALSE` blamed `NOT`."""
    assert _answer(dsn, query) == expected
