"""An array column sorts by `array_cmp` (RP-722), and gap tests of what
working it found.

PostgreSQL orders arrays by `array_cmp` (arrayfuncs.c): the elements first,
in row-major order whatever the shapes, then the one with fewer elements,
fewer dimensions, shorter dimensions, and last the lower bounds. This server
holds an array whose lower bound is not 1 as a tagged document, and the
sort had no comparison between one and a plain array: every such pair
compared equal, so an ORDER BY -- a GROUP BY's, a DISTINCT's, a plain one,
ASC or DESC -- over a column holding both came out scrambled. A plain array
of two dimensions was compared row by row, not element by element.

Found while working it (each a gap test below):

- a WHERE range filter over an array column (`a > '{b}'`, `>=`,
  `BETWEEN`) misses a row whose lower bound is not 1 (RP-733);
- an array column compared with an untyped literal in the select list
  (`select a > '{b}' from t`) is 42883 `operator does not exist: text[] >
  text`, where PostgreSQL reads the literal as `text[]` (RP-734).

The VALUES-only answers were measured on PostgreSQL 15.19 (the shared
reference on port 5415). The ones over `x722` were measured on a private,
freshly initdb'd 15.19 cluster with `_SETUP`.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "drop table if exists x722",
    "create table x722 (k int primary key, a text[])",
    "insert into x722 values (1, '{ab}'), (2, '{cd,ab}'), (5, '[0:1]={x,y}')",
]


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    dsn = _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP:
            c.execute(q)
    yield dsn
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute(_SETUP[0])


def _answer(dsn: str, query: str) -> object:
    """The rows, or the error's SQLSTATE and first line."""
    with psycopg.connect(dsn, autocommit=True) as c:
        try:
            return c.execute(query).fetchall()
        except psycopg.Error as e:
            return (e.sqlstate, str(e).splitlines()[0])


_TEXT = "(values ('{ab}'::text[]), ('{cd,ab}'), ('[0:1]={x,y}'), ('[2:2]={ab}'), ('[0:0]={ab}'))"
_INT = (
    "(values ('{{1,2},{3,4}}'::int[]), ('{{1,2,3}}'), ('{1,2,3}'), ('{1,2,3,4}'),"
    " ('{{1},{2},{3},{4}}'), ('[0:1][1:2]={{1,2},{3,4}}'), ('{}'), ('{1,null}'),"
    " ('{null}'), ('{{1,2},{3,null}}'), ('[1:1][0:3]={{1,2,3,4}}'))"
)
_INT_ASC = [
    "{}",
    "{1,2,3}",
    "{{1,2,3}}",
    "{1,2,3,4}",
    "[1:1][0:3]={{1,2,3,4}}",
    "[0:1][1:2]={{1,2},{3,4}}",
    "{{1,2},{3,4}}",
    "{{1},{2},{3},{4}}",
    "{{1,2},{3,NULL}}",
    "{1,NULL}",
    "{NULL}",
]


@pytest.mark.backlog_fixed("RP-722")
@pytest.mark.parametrize(
    ("query", "rows"),
    [
        (
            f"select a::text t from {_TEXT} v(a) order by a desc",
            ["[0:1]={x,y}", "{cd,ab}", "[2:2]={ab}", "{ab}", "[0:0]={ab}"],
        ),
        (
            f"select a::text t from {_TEXT} v(a) order by a",
            ["[0:0]={ab}", "{ab}", "[2:2]={ab}", "{cd,ab}", "[0:1]={x,y}"],
        ),
        (f"select a::text t from {_INT} v(a) order by a", _INT_ASC),
        (f"select a::text t from {_INT} v(a) order by a desc", _INT_ASC[::-1]),
        (f"select a::text t from {_TEXT} v(a) order by a limit 2", ["[0:0]={ab}", "{ab}"]),
    ],
)
def test_arrays_sort_by_array_cmp(query: str, rows: list, dsn: str) -> None:
    """Elements, then their count, the dimensions and the lower bounds."""
    assert _answer(dsn, query) == [(r,) for r in rows]


@pytest.mark.backlog_fixed("RP-722")
def test_grouped_arrays_with_bounds_sort_by_array_cmp(dsn: str) -> None:
    query = (
        "select a::text t, count(*) from (values ('{ab}'::text[]), ('{cd,ab}'),"
        " ('[0:1]={x,y}'), ('[2:2]={ab}'), ('[0:0]={ab}'), ('{ab}')) v(a)"
        " group by a order by a"
    )
    assert _answer(dsn, query) == [
        ("[0:0]={ab}", 1),
        ("{ab}", 2),
        ("[2:2]={ab}", 1),
        ("{cd,ab}", 1),
        ("[0:1]={x,y}", 1),
    ]


@pytest.mark.backlog_fixed("RP-722")
@pytest.mark.parametrize(
    ("query", "rows"),
    [
        (
            "select count(*), a::text t from x722 group by a order by a",
            [(1, "{ab}"), (1, "{cd,ab}"), (1, "[0:1]={x,y}")],
        ),
        (
            "select a::text t from x722 group by a order by a desc",
            [("[0:1]={x,y}",), ("{cd,ab}",), ("{ab}",)],
        ),
        ("select a::text t from x722 order by a desc", [("[0:1]={x,y}",), ("{cd,ab}",), ("{ab}",)]),
        ("select max(a)::text, min(a)::text from x722", [("[0:1]={x,y}", "{ab}")]),
    ],
)
def test_a_stored_array_column_sorts_by_array_cmp(query: str, rows: list, dsn: str) -> None:
    assert _answer(dsn, query) == rows


@pytest.mark.backlog("RP-733")
@pytest.mark.parametrize(
    ("where", "rows"),
    [
        ("a > '{b}'", ["{cd,ab}", "[0:1]={x,y}"]),
        ("a >= '{cd,ab}'", ["{cd,ab}", "[0:1]={x,y}"]),
        ("a between '{b}' and '{y}'", ["{cd,ab}", "[0:1]={x,y}"]),
        ("a < '{x}'", ["{ab}", "{cd,ab}"]),
        ("a = '[0:1]={x,y}'", ["[0:1]={x,y}"]),
        ("a = '{x,y}'", []),
    ],
)
def test_a_range_filter_finds_an_array_whose_lower_bound_is_not_1(
    where: str, rows: list, dsn: str
) -> None:
    """`array_gt` and its kin compare by `array_cmp`, the bounds last."""
    query = f"select a::text t from x722 where {where} order by k"
    assert _answer(dsn, query) == [(r,) for r in rows]


@pytest.mark.backlog("RP-734")
def test_an_array_column_against_an_untyped_literal_in_the_select_list(dsn: str) -> None:
    """The literal is read as the column's array type, as in a WHERE."""
    assert _answer(dsn, "select k, a > '{b}' from x722 order by k") == [
        (1, False),
        (2, True),
        (5, True),
    ]
    assert _answer(dsn, "select k, a = '{x,y}', a < '{x}' from x722 order by k") == [
        (1, False, True),
        (2, False, True),
        (5, False, False),
    ]
