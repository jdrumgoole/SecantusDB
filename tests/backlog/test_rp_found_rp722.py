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

Found while working it, and since fixed:

- a WHERE comparison of an array column with a constant (`a > '{b}'`,
  `=`, `<>`, BETWEEN, IN, IS DISTINCT FROM) was an MQL filter, which
  missed a row whose lower bound is not 1, matched a two-dimensional array
  holding the operand as a row (`a = '{x,y}'` found `{{x,y},{z,w}}`) and
  sorted a NULL element first; it is evaluated per row by `array_cmp` now
  (RP-733);
- an array column compared with an untyped literal in the select list
  (`select a > '{b}' from t`) was 42883 `operator does not exist: text[] >
  text` once a row held an array whose lower bound is not 1; the literal is
  read as the column's array type now, as PostgreSQL does (RP-734);
- `a = any (array['{x,y}'::text[]])` over a `text[]` column answered rows
  where PostgreSQL resolves `text[] = text` and refuses it, 42883, and ANY
  / ALL compared a scalar with a multidimensional array's sub-arrays, not
  its elements; ANY / ALL take the array's element type and walk every
  element now (RP-740);
- `a IS NULL` over an array column matched an array holding a NULL
  element, through the MQL `{a: null}`; it is evaluated per row now, in a
  WHERE and in an aggregate's FILTER, where an array comparison had been
  0A000 since RP-733 (RP-739).

Found while working those (a gap test below):

- a scalar compared with an array value names `int4 = int4[]` where
  PostgreSQL names `integer = integer[]` (RP-742).

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


@pytest.mark.backlog_fixed("RP-733")
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
    """`array_gt` and its kin compared by `array_cmp`, the bounds last, once
    the comparison was taken off the MQL filter."""
    query = f"select a::text t from x722 where {where} order by k"
    assert _answer(dsn, query) == [(r,) for r in rows]


@pytest.mark.backlog_fixed("RP-734")
def test_an_array_column_against_an_untyped_literal_in_the_select_list(dsn: str) -> None:
    """The literal was read as the column's array type, as in a WHERE."""
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


# Measured on PostgreSQL 15.19 (the shared reference on port 5415) over the
# same rows.
_SHAPES = [
    "drop table if exists x733",
    "create table x733 (k int primary key, a text[], b int[])",
    "insert into x733 values (1, '{{x,y},{z,w}}', '{}'), (2, '{x,NULL}', '{NULL,1}'),"
    " (3, '{x}', '{1,2}'), (4, '{{a,b}}', '{{1,2},{3,4}}'), (5, '[0:1]={x,y}', '[2:3]={1,2}'),"
    " (6, '{x,y}', null), (7, null, '{NULL}')",
]


@pytest.fixture(scope="module")
def shapes(dsn: str) -> Iterator[str]:
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SHAPES:
            c.execute(q)
    yield dsn
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute(_SHAPES[0])


@pytest.mark.backlog_fixed("RP-733")
@pytest.mark.parametrize(
    ("where", "keys"),
    [
        ("a = '{x,y}'", [6]),
        ("a <> '{x,y}'", [1, 2, 3, 4, 5]),
        ("a > '{b}'", [1, 2, 3, 5, 6]),
        ("a >= '{x,y}'", [1, 2, 6]),
        ("a <= '{x,z}'", [1, 3, 4, 5, 6]),
        ("a between '{b}' and '{x,z}'", [1, 3, 5, 6]),
        ("a not between '{b}' and '{x,z}'", [2, 4]),
        ("a in ('{x,y}', '{x}')", [3, 6]),
        ("a not in ('{x,y}', '{x}')", [1, 2, 4, 5]),
        ("a is distinct from '{x,y}'", [1, 2, 3, 4, 5, 7]),
        ("a is not distinct from '{x,y}'", [6]),
        ("a is not distinct from null", [7]),
        ("a = array['x', 'y']", [6]),
        ("a > '{x}' and k > 0", [1, 2, 5, 6]),
        ("b > '{1}'", [2, 3, 4, 5, 7]),
        ("b < '{1}'", [1]),
        ("b = '{}'", [1]),
        ("b > '{NULL,0}'", [2]),
        ("b >= '{1,2}'", [2, 3, 4, 5, 7]),
    ],
)
def test_an_array_column_compares_by_array_cmp(where: str, keys: list, shapes: str) -> None:
    """Every comparison of an array column with a constant: two dimensions
    element by element, a NULL element after every value, the bounds last."""
    assert _answer(shapes, f"select k from x733 where {where} order by k") == [(k,) for k in keys]


@pytest.mark.backlog_fixed("RP-733")
def test_a_write_filters_an_array_column_by_array_cmp(shapes: str) -> None:
    with psycopg.connect(shapes) as c:
        try:
            assert c.execute("update x733 set k = k + 100 where b >= '{1,2}'").rowcount == 5
            assert c.execute("delete from x733 where a > '{x}'").rowcount == 4
            left = c.execute("select k from x733 order by k").fetchall()
        finally:
            c.rollback()
    assert left == [(103,), (104,), (107,)]


@pytest.mark.backlog_fixed("RP-739")
@pytest.mark.parametrize(
    ("where", "keys"),
    [
        ("b is null", [6]),
        ("b is not null", [1, 2, 3, 4, 5, 7]),
        ("b isnull", [6]),
        ("b notnull", [1, 2, 3, 4, 5, 7]),
        ("not (b is null)", [1, 2, 3, 4, 5, 7]),
        ("not (b is not null)", [6]),
        ("b is null and k > 0", [6]),
    ],
)
def test_a_null_test_over_an_array_column_reads_the_array(
    where: str, keys: list, shapes: str
) -> None:
    """An array holding a NULL element was not NULL (`{NULL,1}`, `{NULL}`):
    the null test had lowered to the MQL `{b: null}`, which also matched
    one, and is now evaluated per row. Measured on PostgreSQL 15.19."""
    assert _answer(shapes, f"select k from x733 where {where} order by k") == [(k,) for k in keys]


@pytest.mark.backlog_fixed("RP-739")
def test_a_write_filters_an_array_column_by_its_null_test(shapes: str) -> None:
    """An UPDATE and a DELETE took the same null test as a SELECT."""
    with psycopg.connect(shapes) as c:
        try:
            assert c.execute("update x733 set k = k + 100 where b is null").rowcount == 1
            assert c.execute("delete from x733 where b is not null").rowcount == 6
            left = c.execute("select k from x733 order by k").fetchall()
        finally:
            c.rollback()
    assert left == [(106,)]


@pytest.mark.backlog_fixed("RP-739")
@pytest.mark.parametrize(
    ("query", "answer"),
    [
        (
            "select count(*) filter (where b is null),"
            " count(*) filter (where b is not null) from x733",
            [(1, 6)],
        ),
        (
            "select array_agg(k order by k) filter (where not (b is null)) from x733",
            [([1, 2, 3, 4, 5, 7],)],
        ),
        (
            "select array_agg(k order by k) filter (where b is null and k > 0) from x733",
            [([6],)],
        ),
        # An array comparison in a FILTER had been 0A000 since RP-733 made
        # it per row in a WHERE.
        (
            "select count(*) filter (where b = '{1,2}'),"
            " count(k) filter (where a > '{x}') from x733",
            [(1, 4)],
        ),
        (
            "select array_agg(k order by k) filter (where b >= '{1,2}') from x733",
            [([2, 3, 4, 5, 7],)],
        ),
        (
            "select k, count(*) filter (where b isnull) over (order by k) from x733 order by k",
            [(1, 0), (2, 0), (3, 0), (4, 0), (5, 0), (6, 1), (7, 1)],
        ),
    ],
)
def test_an_aggregate_filter_over_an_array_column_is_per_row(
    query: str, answer: list, shapes: str
) -> None:
    """An aggregate's FILTER the MQL filter cannot express -- an array
    column's null test or comparison -- was refused as 0A000 (or, for the
    null test, matched an array holding a NULL); it is evaluated per row as
    a WHERE's residual is. Measured on PostgreSQL 15.19 over the same rows
    as a VALUES list."""
    assert _answer(shapes, query) == answer


@pytest.mark.backlog_fixed("RP-740")
def test_an_array_column_against_any_of_an_array_of_arrays(shapes: str) -> None:
    """`array['{x,y}'::text[]]` is a `text[]`, so ANY compared `text[]` with
    `text`, which has no operator; the filter had matched it as `a = '{x,y}'`."""
    assert _answer(shapes, "select k from x733 where a = any (array['{x,y}'::text[]])") == (
        "42883",
        "operator does not exist: text[] = text",
    )


@pytest.mark.backlog_fixed("RP-740")
@pytest.mark.parametrize(
    ("query", "answer"),
    [
        # ANY / ALL walk every element of a multidimensional array, as
        # `ExecEvalScalarArrayOp` does; a row holding `{{1,2},{3,4}}` was
        # 42883 `int4 = int4[]` for `2 = any (b)`.
        ("select k from x733 where 2 = any (b) order by k", [(3,), (4,), (5,)]),
        ("select k from x733 where 'w' = any (a) order by k", [(1,)]),
        (
            "select k, 4 = any (b), 1 <> all (b) from x733 order by k",
            [
                (1, False, True),
                (2, None, False),
                (3, False, False),
                (4, True, False),
                (5, False, False),
                (6, None, None),
                (7, None, None),
            ],
        ),
        ("select 1 = any ('{{2},{1}}'::int[]), 2 = all ('{{2},{2}}'::int[])", [(True, True)]),
        # An array column meets the array's element type.
        (
            "select k from x733 where a = any (array[a])",
            ("42883", "operator does not exist: text[] = text"),
        ),
        (
            "select k from x733 where a <> all ('{{x,y}}'::text[])",
            ("42883", "operator does not exist: text[] <> text"),
        ),
        (
            "select k from x733 where a = any ('{x}')",
            ("42704", "could not find array type for data type text[]"),
        ),
        (
            "select '{x,y}'::text[] = any (array['{x,y}'::text[]])",
            ("42883", "operator does not exist: text[] = text"),
        ),
        (
            "select k from (values (1, '{x,y}'::text[])) v(k, a)"
            " where a = any (array['{x,y}'::text[]])",
            ("42883", "operator does not exist: text[] = text"),
        ),
        # A subquery's rows that are arrays stay whole, and a vector array's
        # elements are vectors.
        (
            "select k from x733 where b = any (select b from x733 where k in (3, 4)) order by k",
            [(3,), (4,)],
        ),
        (
            "select k from x733 where b <> all (select b from x733 where k = 3) order by k",
            [(1,), (2,), (4,), (5,), (7,)],
        ),
        ("select array[1] = any (select array[1])", [(True,)]),
        ("select '1 2'::int2vector = any (array['1 2'::int2vector])", [(True,)]),
    ],
)
def test_any_and_all_compare_with_the_arrays_elements(
    query: str, answer: object, shapes: str
) -> None:
    """Measured on PostgreSQL 15.19 over the same rows (`x733` as a CTE)."""
    assert _answer(shapes, query) == answer


@pytest.mark.backlog("RP-742")
@pytest.mark.parametrize(
    "query",
    [
        "select 1 = (select array[1])",
        "select x = y from (values (1, array[1])) v(x, y)",
        "select 1 = any (select array[1])",
    ],
)
def test_a_scalar_against_an_array_value_names_its_types(query: str, dsn: str) -> None:
    """PostgreSQL 15.19 names `integer` and `integer[]`, not `int4`."""
    assert _answer(dsn, query) == ("42883", "operator does not exist: integer = integer[]")
