"""What working RP-775 (a VALUES list's `char(n)` column in a WHERE) found:

- evaluated per row, a `char(n)` operand against a `text` one resolves
  the operator wrongly in two shapes: a longer IN list's `text` member,
  and a scalar subquery's `text` value (RP-792);
- a one-member IN list of `text` over a stored `char(n)` column compares
  as `bpchar`, where PostgreSQL makes it a plain `=`, `text`'s (RP-793);
- an ORDER BY over an unsized `bpchar` column of a VALUES list or a set
  operation counts the cells' trailing blanks (RP-794).

The stored-table answers were measured on a private, freshly initdb'd
PostgreSQL 15.19 cluster with `_SETUP`; the VALUES-only ones on the shared
15.19 reference (port 5415), as written.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "drop table if exists t793",
    "create table t793 (f char(4))",
    "insert into t793 values ('uu'), ('tt'), ('vv')",
]


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        c.execute(_SETUP[0])


def _answer(dsn: str, query: str, setup: list[str] | None = None) -> object:
    """The rows, or the error's SQLSTATE and first line."""
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in setup or []:
            c.execute(q)
        try:
            return [tuple(r) for r in c.execute(query).fetchall()]
        except psycopg.Error as e:
            return ("error", e.sqlstate, str(e).splitlines()[0])


_C = "from (values ('uu'::char(4)), ('tt'), ('vv')) s(f)"


@pytest.mark.backlog("RP-792")
@pytest.mark.parametrize(
    ("query", "expected"),
    [
        # More than one member: `transformAExprIn` takes the list's common
        # type, `bpchar` (the first, as `text` and `bpchar` coerce both
        # ways), so the `text` member's blanks do not count.
        (f"select f {_C} where f in ('uu  '::text, 'tt') order by f", [("tt",), ("uu  ",)]),
        (
            f"select f, f in ('uu  '::text, 'tt'::text) {_C} order by f",
            [("tt", True), ("uu  ", True), ("vv", False)],
        ),
        # `bpchar = text` resolves to text: the column is rtrimmed and the
        # subquery's text keeps its blanks, so nothing matches.
        (f"select f {_C} where f = (select 'uu  ') order by f", []),
        (
            f"select f, f = (select 'uu  '::text) {_C} order by f",
            [("tt", False), ("uu  ", False), ("vv", False)],
        ),
    ],
)
def test_a_char_operand_against_text_per_row(dsn: str, query: str, expected: object) -> None:
    """Evaluated per row -- a select list, or a WHERE over a VALUES list --
    a `char(n)` operand compares with a `text` member of a longer IN list
    as `text`, and with a scalar subquery's `text` value as `bpchar`
    (blanks dropped); PostgreSQL 15.19 does the opposite in each."""
    assert _answer(dsn, query) == expected


@pytest.mark.backlog("RP-793")
@pytest.mark.parametrize(
    ("query", "expected"),
    [
        ("select f from t793 where f in ('uu  '::text) order by f", []),
        (
            "select f from t793 where f not in ('uu  '::text) order by f",
            [("tt  ",), ("uu  ",), ("vv  ",)],
        ),
    ],
)
def test_a_one_member_text_in_list_compares_as_text(dsn: str, query: str, expected: object) -> None:
    """`transformAExprIn` builds a `bpchar[]` comparison only for more
    than one non-Var member; one member is `f = 'uu  '::text`, which
    resolves to `texteq` over the `rtrim`med column, so nothing matches on
    PostgreSQL 15.19. Here the member is coerced to `bpchar` and its blanks
    dropped, so `uu` matches (and NOT IN drops it)."""
    assert _answer(dsn, query, _SETUP) == expected


@pytest.mark.backlog("RP-794")
@pytest.mark.parametrize(
    ("query", "expected"),
    [
        (
            "select f from (values ('a  '::char(3)), (E'a\\t')) s(f) order by f",
            [("a  ",), ("a\t",)],
        ),
        (
            "select f from (values ('a  '::char(3)), (E'a\\t')) s(f) order by f desc",
            [("a\t",), ("a  ",)],
        ),
        (
            "select f from (select 'a'::char(3) f union all select E'a\\t') s order by f",
            [("a  ",), ("a\t",)],
        ),
    ],
)
def test_an_unsized_bpchar_column_sorts_without_its_blanks(
    dsn: str, query: str, expected: object
) -> None:
    """`bpcharcmp` compares the values' `bcTruelen` lengths, so `'a  '`
    sorts as `'a'`, before `E'a\\t'`, on PostgreSQL 15.19. The column is
    unsized (its members' modifiers differ), so its cells keep their
    blanks here, and the sort reads them: a blank (0x20) sorts after a tab
    (0x09)."""
    assert _answer(dsn, query) == expected
