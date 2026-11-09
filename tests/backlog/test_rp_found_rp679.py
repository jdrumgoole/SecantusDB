"""What working RP-679 (a `char(n)` value that carries its blanks compares and
sorts with them) pinned and found:

- RP-679 (fixed): a FROM-subquery -- a VALUES list of `char(n)` values among
  them -- stored a padded `char(n)` cell padded, so the outer filter compared
  it with its blanks and found nothing; a computed ORDER BY key, a window's
  and an aggregate's ORDER BY sorted a `char(n)` key with its blanks
  (`bpcharcmp` ignores them, `bcTruelen`); and a correlated scalar subquery
  was described with no modifier, where `exprTypmod` of an EXPR sublink is
  its column's.
- a correlated `ARRAY(...)` over a `char(n)` column answers its elements
  without their blanks (RP-683).

Measured on PostgreSQL 15.19 (the shared reference on port 5415): every
statement is VALUES-only and runs there as written.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

V = "(values (1, 'a'::char(3)), (2, 'xy'::char(3)), (3, E'a\\t'::char(3)))"


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"


def _answer(dsn: str, query: str) -> object:
    """Each column's modifier and the rows, or the error's SQLSTATE and first line."""
    with psycopg.connect(dsn, autocommit=True) as c:
        try:
            cur = c.execute(query)
            mods = [cur.pgresult.fmod(i) for i in range(cur.pgresult.nfields)]
            return mods, cur.fetchall()
        except psycopg.Error as e:
            return ("error", e.sqlstate, str(e).splitlines()[0])


# Each answered differently before RP-679: the filters found nothing, the
# sorts put the tab first, and the scalar subquery was described `-1`.
FIXED = {
    f"select k from {V} s(k, f) where f = 'xy'": ([-1], [(2,)]),
    f"select k from (select k, f::char(5) x from {V} s(k, f)) q"
    " where x in ('a', 'xy') order by k": (
        [-1],
        [(1,), (2,)],
    ),
    f"select k, x, octet_length(x), x || '|' from (select k, f::char(5) x from {V} s(k, f)) q"
    " order by k": (
        [-1, 9, -1, -1],
        [(1, "a    ", 5, "a|"), (2, "xy   ", 5, "xy|"), (3, "a\t   ", 5, "a\t|")],
    ),
    f"select k from (select k, f::char(5) x from {V} s(k, f)) q order by x, k": (
        [-1],
        [(1,), (3,), (2,)],
    ),
    f"select k from {V} s(k, f) order by f::char(5), k": ([-1], [(1,), (3,), (2,)]),
    f"select k, row_number() over (order by f::char(5)) from {V} s(k, f) order by k": (
        [-1, -1],
        [(1, 1), (2, 3), (3, 2)],
    ),
    f"select string_agg(k::text, ',' order by f::char(5)) from {V} s(k, f)": ([-1], [("1,3,2",)]),
    f"select k, (select s.f) x from {V} s(k, f) order by k": (
        [-1, 7],
        [(1, "a  "), (2, "xy "), (3, "a\t ")],
    ),
    f"select k from (select k, (select s.f) x from {V} s(k, f)) q where x = 'xy'": ([-1], [(2,)]),
}


@pytest.mark.backlog_fixed("RP-679")
@pytest.mark.parametrize("query", list(FIXED))
def test_char_n_cells_and_sort_keys_compare_without_their_blanks(query: str, dsn: str) -> None:
    assert _answer(dsn, query) == FIXED[query]


@pytest.mark.backlog("RP-683")
def test_correlated_array_of_char_n_keeps_the_elements_blanks(dsn: str) -> None:
    query = (
        f"select k, array(select t.f from {V} t(k, f) where t.f = s.f) from {V} s(k, f) order by k"
    )
    assert _answer(dsn, query) == (
        [-1, 7],
        [(1, ["a  "]), (2, ["xy "]), (3, ["a\t "])],
    )
