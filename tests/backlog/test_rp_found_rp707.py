"""What working RP-707 (a `text` scalar subquery is typed `text`) found:

- a scalar subquery whose column is an untyped literal (`(select 'a')`)
  was still read as `unknown` by the expression around it, so it was
  coerced to the other side's type, where PostgreSQL resolves the
  subquery's column to `text` and finds no operator (RP-756, fixed);
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


@pytest.mark.backlog_fixed("RP-756")
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
        # A NULL is resolved to text as well.
        (
            "select (select null) = 1",
            ("error", "42883", "operator does not exist: text = integer"),
        ),
        (
            "select (select '2020-01-01') = date '2020-01-01'",
            ("error", "42883", "operator does not exist: text = date"),
        ),
        (
            "select coalesce(1, (select 'a'))",
            ("error", "42804", "COALESCE types integer and text cannot be matched"),
        ),
        (
            "select greatest((select '1'), 2)",
            ("error", "42804", "GREATEST types text and integer cannot be matched"),
        ),
        (
            "select nullif((select '1'), 1)",
            ("error", "42883", "operator does not exist: text = integer"),
        ),
        (
            "select x from (values (1)) v(x) where x = (select '1')",
            ("error", "42883", "operator does not exist: integer = text"),
        ),
        (
            "select (select '1') in (1, 2)",
            ("error", "42883", "operator does not exist: text = integer"),
        ),
        # Through a FROM subquery inside it, and a set operation of
        # untyped arms.
        (
            "select (select x from (select 'a') s(x)) = 1",
            ("error", "42883", "operator does not exist: text = integer"),
        ),
        (
            "select (select 'a' union select 'b' limit 1) = 1",
            ("error", "42883", "operator does not exist: text = integer"),
        ),
        # A FROM subquery's and a CTE's untyped column are text too.
        (
            "select x = 1 from (select null) s(x)",
            ("error", "42883", "operator does not exist: text = integer"),
        ),
        (
            "with c as (select 'a' as x) select x = 1 from c",
            ("error", "42883", "operator does not exist: text = integer"),
        ),
        # What a text value may do, it still does.
        ("select (select '1') = '1'", ([16], [(True,)])),
        ("select x from (select '1') s(x) where x = '1'", ([25], [("1",)])),
        ("select coalesce(x, 'z') from (select null) s(x)", ([25], [("z",)])),
        ("select (select '1')::int + 1", ([23], [(2,)])),
    ],
)
def test_an_untyped_literal_subquery_is_text_outside_it(
    query: str, expected: object, dsn: str
) -> None:
    """PostgreSQL 15.19 resolves an untyped literal in a subquery's target
    list to `text` (`resolveTargetListUnknowns`), so the subquery is a
    `text` value to the expression around it. Here the outer expression
    coerced it as `unknown`: `1 = (select '1')` and `(select 'a') = 1`
    were 0A000 `comparing ... is not supported yet`, the `< now()`
    answered true, and a CASE cast it to `integer` (22P02). The operator
    check now types the subquery, and a FROM subquery's or CTE's untyped
    column, as `text` (RP-756)."""
    assert _answer(dsn, query) == expected


@pytest.mark.backlog_fixed("RP-757")
def test_a_scalar_subquery_in_an_in_list(dsn: str) -> None:
    """A scalar subquery as one element of an `IN` list is an ordinary
    expression on PostgreSQL 15.19 (`x = sub OR x = 'tt'`); the statement
    was 0A000 `SubLink is not supported yet`, as the subquery resolver did
    not descend into the list."""
    assert _answer(
        dsn,
        "select x from (values ('tt'::text), ('uu'), ('vv')) s(x)"
        " where x in ((select max(y) from (values ('uu'::text)) t(y)), 'tt') order by x",
    ) == ([25], [("tt",), ("uu",)])


_S = "from (values ('tt'::text), ('uu'), ('vv')) s(x)"


@pytest.mark.backlog_fixed("RP-757")
@pytest.mark.parametrize(
    ("query", "expected"),
    [
        (
            f"select x {_S} where x not in ((select max(y) from (values ('uu'::text)) t(y)), 'tt')"
            " order by x",
            ([25], [("vv",)]),
        ),
        # A NULL member: never true in IN, makes NOT IN never true.
        (f"select x {_S} where x in ((select null::text), 'tt') order by x", ([25], [("tt",)])),
        (f"select x {_S} where x not in ((select null::text), 'tt') order by x", ([25], [])),
        # No row: the member is NULL.
        (
            f"select x {_S} where x in"
            " ((select y from (values ('uu'::text)) t(y) where false), 'tt') order by x",
            ([25], [("tt",)]),
        ),
        (
            f"select x {_S} where x in ((select y from (values ('uu'::text), ('vv')) t(y)), 'tt')"
            " order by x",
            ("error", "21000", "more than one row returned by a subquery used as an expression"),
        ),
        # Correlated: evaluated per outer row.
        (
            f"select x {_S} where x in ((select y from (values ('uu'::text), ('vv')) t(y)"
            " where y = s.x), 'tt') order by x",
            ([25], [("tt",), ("uu",), ("vv",)]),
        ),
        (
            f"select x {_S} where x in ((select 1), 'tt') order by x",
            ("error", "42883", "operator does not exist: text = integer"),
        ),
        (
            "select x from (values (1), (2), (3)) s(x) where x in ((select 2), 3.0) order by x",
            ([23], [(2,), (3,)]),
        ),
        (
            f"select x {_S} where x in ((select 'uu'), (select 'vv')) order by x",
            ([25], [("uu",), ("vv",)]),
        ),
        (
            "select 'tt' in ((select 'tt'), 'x'), 'q' in ((select 'tt'), 'x')",
            ([16, 16], [(True, False)]),
        ),
        (
            f"select x, x in ((select 'uu'), 'tt') {_S} order by x",
            ([25, 16], [("tt", True), ("uu", True), ("vv", False)]),
        ),
    ],
)
def test_a_scalar_subquery_in_an_in_list_shapes(dsn: str, query: str, expected: object) -> None:
    """More `IN` lists with a scalar subquery member, each measured on
    PostgreSQL 15.19 (the shared reference on port 5415)."""
    assert _answer(dsn, query) == expected
