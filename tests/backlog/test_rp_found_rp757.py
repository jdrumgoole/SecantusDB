"""What working RP-757 (a scalar subquery in an `IN` list) found:

- a scalar subquery under `IS [NOT] TRUE` / `FALSE` / `UNKNOWN`, or under
  a `COLLATE`, is still refused;
- a `char(n)` column of a VALUES list, compared in a WHERE, did not
  compare as `bpchar` (RP-775, fixed).

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
    """The rows, or the error's SQLSTATE and first line."""
    with psycopg.connect(dsn, autocommit=True) as c:
        try:
            return [tuple(r) for r in c.execute(query).fetchall()]
        except psycopg.Error as e:
            return ("error", e.sqlstate, str(e).splitlines()[0])


_S = "from (values ('tt'::text), ('uu'), ('vv')) s(x)"


@pytest.mark.backlog("RP-774")
@pytest.mark.parametrize(
    ("query", "expected"),
    [
        (f"select x {_S} where (x = (select 'uu')) is not true order by x", [("tt",), ("vv",)]),
        (f"select x {_S} where (x = (select 'uu')) is true order by x", [("uu",)]),
        (f"select x {_S} where x = (select 'uu') collate \"C\" order by x", [("uu",)]),
    ],
)
def test_a_scalar_subquery_under_a_boolean_test_or_collate(
    dsn: str, query: str, expected: object
) -> None:
    """A scalar subquery under `IS [NOT] TRUE` or `COLLATE` is an ordinary
    value on PostgreSQL 15.19; here each statement is 0A000 `SubLink is not
    supported yet`."""
    assert _answer(dsn, query) == expected


_C = "from (values ('uu'::char(4)), ('tt'), ('vv')) s(f)"
# A set operation whose sides' modifiers differ is unsized `bpchar` too.
_U = "from (select 'uu'::char(4) f union all select 'tt' union all select 'vv') s"


@pytest.mark.backlog_fixed("RP-775")
@pytest.mark.parametrize(
    ("query", "expected"),
    [
        # An IN list's members take the column's type, bpchar: blanks
        # do not count.
        (f"select f {_C} where f in ('uu  ', 'tt') order by f", [("tt",), ("uu  ",)]),
        (f"select f {_C} where f in ((select 'uu'), 'tt') order by f", [("tt",), ("uu  ",)]),
        (f"select f {_C} where f not in ('uu  ', 'tt') order by f", [("vv",)]),
        (f"select f {_C} where f = 'uu' order by f", [("uu  ",)]),
        (f"select f {_C} where f <> 'uu  ' order by f", [("tt",), ("vv",)]),
        (f"select f {_C} where f <= 'uu' order by f", [("tt",), ("uu  ",)]),
        (f"select f {_C} where f between 'tt ' and 'uu ' order by f", [("tt",), ("uu  ",)]),
        (f"select f {_C} where f is distinct from 'uu  ' order by f", [("tt",), ("vv",)]),
        # Against `text` the column is `rtrim`med and the text keeps its
        # blanks.
        (f"select f {_C} where f = 'uu  '::text order by f", []),
        (f"select f {_C} where f = 'uu'::text order by f", [("uu  ",)]),
        # A one-member list is a plain `=`, here `text`'s.
        (f"select f {_C} where f in ('uu  '::text) order by f", []),
        (f"select f {_U} where f = 'uu' order by f", [("uu  ",)]),
        (f"select f {_U} where f = 'uu  '::text order by f", []),
        (
            "with s(f) as (values ('uu'::char(4)), ('tt')) select f from s where f = 'uu'",
            [("uu  ",)],
        ),
        (
            "select s.f from (values ('uu'::char(4)), ('tt')) s(f), (values (1)) t(n)"
            " where s.f = 'uu'",
            [("uu  ",)],
        ),
    ],
)
def test_a_values_char_column_compares_as_bpchar_in_a_where(
    dsn: str, query: str, expected: object
) -> None:
    """An unsized `bpchar` column of a VALUES list or a set operation
    keeps its cells' blanks (`'uu  '`, as PostgreSQL 15.19 shows them), and
    a WHERE over it read them: `f = 'uu'` and `f in ('uu  ', 'tt')` found
    nothing, `f = 'uu  '::text` found `'uu  '`. Such a predicate is now
    evaluated per row, as `bpchar` compares."""
    assert _answer(dsn, query) == expected
