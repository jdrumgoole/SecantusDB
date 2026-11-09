"""What working RP-517 (`v.*` in a subquery over an outer FROM item) found:
the shapes of a correlated subquery that fail with a column named
explicitly as well as through a star, so they are not the star's:

- a row compared with a correlated subquery (`(a, c) in (select fs.a,
  fs.c)`) was refused (RP-697, fixed);
- a correlated set operation is 42P01 (RP-698);
- a correlated subquery that passed an outer FROM function's column, or a
  field of an outer whole row, through unchanged answered it as text, and
  `(v).*` there was refused (RP-699, fixed);
- a GROUP BY naming a correlated subquery target is 42803 (RP-700);
- a LATERAL subquery's outer column is named by its type, and a star over
  an outer item there is 42P01 (RP-701).

An answer is the rows and the column names, or `(sqlstate, message,
position)` for an error. Measured on a private, freshly initdb'd PostgreSQL
15.19 cluster with `_SETUP`.
"""

from __future__ import annotations

from collections.abc import Iterator
from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "drop table if exists fe, fs cascade",
    "create table fe (a int, b text)",
    "create table fs (a int, c text)",
    "insert into fe values (1, 'x')",
    "insert into fs values (1, 'y')",
]


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        c.execute(_SETUP[0])


def answer(dsn: str, query: str) -> Any:
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP:
            c.execute(q)
        try:
            cur = c.execute(query)
        except psycopg.Error as e:
            return (e.sqlstate, e.diag.message_primary, e.diag.statement_position)
        return (cur.fetchall(), [d.name for d in cur.description or []])


# `transformAExprIn` / `make_row_comparison_op` compare the row with the
# correlated subquery's columns pairwise. Each was 0A000 `comparing
# document with int32 using = is not supported yet`, with no position
# (RP-697, fixed).
ROW_AGAINST_CORRELATED = {
    "select a from fs where (a, c) in (select fs.a, fs.c)": ([(1,)], ["a"]),
    "select a from fs where (a, c) = (select fs.a, fs.c)": ([(1,)], ["a"]),
    "select a from fs where (a, c) not in (select fs.a, fs.c)": ([], ["a"]),
}


@pytest.mark.backlog_fixed("RP-697")
@pytest.mark.parametrize("query", list(ROW_AGAINST_CORRELATED))
def test_row_against_a_correlated_subquery(query: str, dsn: str) -> None:
    assert answer(dsn, query) == ROW_AGAINST_CORRELATED[query]


# More rows for the row comparisons: NULLs in either row, every operator,
# `char(n)` pairs, and the uncorrelated `row op (subquery)`, which compared
# the row with the subquery's first column (0A000 `comparing document with
# int32`) in a select list and a WHERE alike. Measured on the same private
# PostgreSQL 15.19 cluster.
_ROW_SETUP = [
    "drop table if exists rn, ch cascade",
    "create table rn (k int, a int, c text)",
    "insert into rn values (1, 1, 'y'), (2, null, 'y'), (3, 2, 'z'), (4, 1, null)",
    "create table ch (k int, c char(3), v varchar(4))",
    "insert into ch values (1, 'y', 'y'), (2, 'ab', 'ab ')",
]

_T, _F = True, False
_COL = ["k", "?column?"]
ROW_COMPARISONS = {
    # IN / NOT IN / ANY / ALL: each subquery row compared pair by pair.
    "select k, (a, c) in (select fs.a, fs.c from fs where fs.a = rn.a) from rn order by k": (
        [(1, _T), (2, _F), (3, _F), (4, None)],
        _COL,
    ),
    "select k, (a, c) not in (select fs.a, fs.c from fs where fs.a = rn.a or rn.k = 4)"
    " from rn order by k": ([(1, _F), (2, _T), (3, _T), (4, None)], _COL),
    "select k, (a, c) = any (select fs.a, fs.c from fs where rn.k < 3) from rn order by k": (
        [(1, _T), (2, None), (3, _F), (4, _F)],
        _COL,
    ),
    "select k, (a, c) <> all (select fs.a, fs.c from fs where rn.k < 3) from rn order by k": (
        [(1, _F), (2, None), (3, _T), (4, _T)],
        _COL,
    ),
    "select k, (a, c) = all (select fs.a, fs.c from fs where rn.k < 3) from rn order by k": (
        [(1, _T), (2, None), (3, _T), (4, _T)],
        _COL,
    ),
    "select k, (a, c) <> any (select fs.a, fs.c from fs where rn.k < 4) from rn order by k": (
        [(1, _F), (2, None), (3, _T), (4, _F)],
        _COL,
    ),
    "select k, (k, a) < any (select rn.k, 3) from rn order by k": (
        [(1, _T), (2, None), (3, _T), (4, _T)],
        _COL,
    ),
    "select k, (k, a) > all (select rn.k - 1, 3) from rn order by k": (
        [(1, _T), (2, _T), (3, _T), (4, _T)],
        _COL,
    ),
    "select k, (a, 'y') in (select fs.a, fs.c from fs where fs.a = rn.a) from rn order by k": (
        [(1, _T), (2, _F), (3, _F), (4, _T)],
        _COL,
    ),
    "select k, row(a, c) in (select rn.a, 'y') from rn order by k": (
        [(1, _T), (2, None), (3, _F), (4, None)],
        _COL,
    ),
    # `row op (subquery)`: one row, no row NULL, compared as a RowCompareExpr.
    "select k, (a, c) = (select fs.a, fs.c from fs where fs.a = rn.a) from rn order by k": (
        [(1, _T), (2, None), (3, None), (4, None)],
        _COL,
    ),
    "select k, (a, c) <> (select fs.a, fs.c from fs where fs.a = rn.a) from rn order by k": (
        [(1, _F), (2, None), (3, None), (4, None)],
        _COL,
    ),
    "select k, (a, c) = (select fs.a, fs.c from fs, fe where rn.k > 0) from rn order by k": (
        [(1, _T), (2, None), (3, _F), (4, None)],
        _COL,
    ),
    "select k, (1, c) = (select rn.a, 'y') from rn order by k": (
        [(1, _T), (2, None), (3, _F), (4, None)],
        _COL,
    ),
    "select k, (k, a) < (select rn.k, 5) from rn order by k": (
        [(1, _T), (2, None), (3, _T), (4, _T)],
        _COL,
    ),
    "select k, (k, a) >= (select rn.k, 1) from rn order by k": (
        [(1, _T), (2, None), (3, _T), (4, _T)],
        _COL,
    ),
    "select k, (k, a) <= (select rn.k, rn.a) from rn order by k": (
        [(1, _T), (2, None), (3, _T), (4, _T)],
        _COL,
    ),
    "select k, (k, a, c) < (select rn.k, rn.a, 'z') from rn order by k": (
        [(1, _T), (2, None), (3, _F), (4, None)],
        _COL,
    ),
    "select k, (k, a, c) >= (select rn.k, rn.a + 1, 'a') from rn order by k": (
        [(1, _F), (2, None), (3, _F), (4, _F)],
        _COL,
    ),
    "select k, (k, c) >= (select rn.k, null::text) from rn order by k": (
        [(1, None), (2, None), (3, None), (4, None)],
        _COL,
    ),
    "select k, (a, k) > (select null::int, rn.k) from rn order by k": (
        [(1, None), (2, None), (3, None), (4, None)],
        _COL,
    ),
    "select k from rn where (a, c) = (select rn.a, 'y') order by k": ([(1,)], ["k"]),
    "select k from rn where (k, a) <= (select 3, rn.a) order by k": (
        [(1,), (2,), (3,)],
        ["k"],
    ),
    # `char(n)` pairs compare as `bpchar`, trailing blanks ignored.
    "select k, (c, k) in (select x.c, x.k from ch x where x.k = ch.k) from ch order by k": (
        [(1, _T), (2, _T)],
        _COL,
    ),
    "select k, (c, k) in (select x.c, x.k from ch x) from ch order by k": (
        [(1, _T), (2, _T)],
        _COL,
    ),
    "select k, (c, k) = (select 'ab'::char(3), ch.k) from ch order by k": (
        [(1, _F), (2, _T)],
        _COL,
    ),
    "select k, (c, k) < (select x.c, x.k + 1 from ch x where x.k = ch.k) from ch order by k": (
        [(1, _T), (2, _T)],
        _COL,
    ),
    "select k, (c, k) > all (select x.c, x.k - 1 from ch x where x.k <= ch.k) from ch order by k": (
        [(1, _T), (2, _F)],
        _COL,
    ),
    # Uncorrelated.
    "select k, (a, c) = (select 1, 'y') from rn order by k": (
        [(1, _T), (2, None), (3, _F), (4, None)],
        _COL,
    ),
    "select k, (k, a) < (select 2, 5) from rn order by k": (
        [(1, _T), (2, None), (3, _F), (4, _F)],
        _COL,
    ),
    "select k, (a, c) = (select 1, 'y' where false) from rn order by k": (
        [(1, None), (2, None), (3, None), (4, None)],
        _COL,
    ),
    "select k from rn where (a, c) = (select 1, 'y') order by k": ([(1,)], ["k"]),
    "select k, (c, k) = (select x.c, x.k from ch x where x.k = 2) from ch order by k": (
        [(1, _F), (2, _T)],
        _COL,
    ),
    "select k, (a, c) = (select 1, 'y' union all select 2, 'z') from rn order by k": (
        "21000",
        "more than one row returned by a subquery used as an expression",
        None,
    ),
}


@pytest.mark.backlog_fixed("RP-697")
@pytest.mark.parametrize("query", list(ROW_COMPARISONS))
def test_row_comparison_with_a_subquery(query: str, dsn: str) -> None:
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _ROW_SETUP:
            c.execute(q)
    try:
        assert answer(dsn, query) == ROW_COMPARISONS[query]
    finally:
        with psycopg.connect(dsn, autocommit=True) as c:
            c.execute(_ROW_SETUP[0])


@pytest.mark.backlog_fixed("RP-697")
def test_row_against_a_correlated_subquery_with_a_parameter(dsn: str) -> None:
    """A prepared statement's parameter read inside the correlated
    subquery: `[(3,)]` on the private PostgreSQL 15.19 cluster."""
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _ROW_SETUP:
            c.execute(q)
        try:
            c.execute(
                "prepare p697(int) as select k from rn where (a, c) in"
                " (select rn.a, rn.c where rn.k > $1) order by k"
            )
            assert c.execute("execute p697(1)").fetchall() == [(3,)]
        finally:
            c.execute(_ROW_SETUP[0])


# A set operation's arms read the outer row as any subquery does. Here each
# is 42P01 `missing FROM-clause entry for table "v"` at 16.
CORRELATED_SET_OPERATION = {
    "select (select v.a union select 9) from (values (5)) v(a)": (
        "21000",
        "more than one row returned by a subquery used as an expression",
        None,
    ),
    "select (select v.a intersect select 5) from (values (5), (6)) v(a)": (
        [(5,), (None,)],
        ["a"],
    ),
    "select (select v.a union all select 9 limit 1) from (values (5)) v(a)": ([(5,)], ["a"]),
}


@pytest.mark.backlog("RP-698")
@pytest.mark.parametrize("query", list(CORRELATED_SET_OPERATION))
def test_correlated_set_operation(query: str, dsn: str) -> None:
    assert answer(dsn, query) == CORRELATED_SET_OPERATION[query]


# The subquery's column is the outer Var, of its type. A FROM function's
# column, or a field of an outer whole row, passed through unchanged
# answered as text (`'1'`), though `f.x + 1` was an integer; and `(v).*`
# was 0A000 `this field selection is not supported yet` (RP-699, fixed).
CORRELATED_PASS_THROUGH_TYPE = {
    "select (select f.x) from generate_series(1, 2) f(x)": ([(1,), (2,)], ["x"]),
    "select (select f) from generate_series(1, 2) f": ([(1,), (2,)], ["f"]),
    "select (select (v).a) from (values (1)) v(a)": ([(1,)], ["a"]),
    "select (select (v).*) from (values (1)) v(a)": ([(1,)], ["a"]),
}


@pytest.mark.backlog_fixed("RP-699")
@pytest.mark.parametrize("query", list(CORRELATED_PASS_THROUGH_TYPE))
def test_correlated_pass_through_keeps_its_type(query: str, dsn: str) -> None:
    assert answer(dsn, query) == CORRELATED_PASS_THROUGH_TYPE[query]


# A GROUP BY item that is the correlated subquery itself (by position or
# written out) groups by it, so its outer column is grouped. Here the
# first is 42803 `subquery uses ungrouped column "v.a" from outer query`,
# the star form 42P01 `missing FROM-clause entry for table "v"` and the
# written-out one 42803 `column "v.a" must appear in the GROUP BY clause`,
# each at 16.
GROUP_BY_CORRELATED_TARGET = {
    "select (select v.a) from (values (1)) v(a) group by 1": ([(1,)], ["a"]),
    "select (select v.*) from (values (1)) v(a) group by 1": ([(1,)], ["a"]),
    "select (select v.a) from (values (1), (1)) v(a) group by (select v.a)": (
        [(1,)],
        ["a"],
    ),
}


@pytest.mark.backlog("RP-700")
@pytest.mark.parametrize("query", list(GROUP_BY_CORRELATED_TARGET))
def test_group_by_a_correlated_subquery_target(query: str, dsn: str) -> None:
    assert answer(dsn, query) == GROUP_BY_CORRELATED_TARGET[query]


# A LATERAL subquery's column is named as any select-list column is
# (`FigureColname`: `v.a` is `a`), and a star there over an earlier FROM
# item is that item's columns (`ExpandColumnRefStar`). Here the names are
# the columns' types (`int4`, `text`), and each star is 42P01 `invalid
# reference to FROM-clause entry`, with the hint that the entry cannot be
# referenced from this part of the query.
LATERAL_OUTER_COLUMNS = {
    "select l.* from (values (3)) v(a), lateral (select v.a) l": ([(3,)], ["a"]),
    "select l.* from fe, lateral (select fe.a, fe.b) l": ([(1, "x")], ["a", "b"]),
    "select l.* from (values (3)) v(a), lateral (select v.*) l": ([(3,)], ["a"]),
    "select l.* from fe, lateral (select fe.* from fs) l": ([(1, "x")], ["a", "b"]),
}


@pytest.mark.backlog("RP-701")
@pytest.mark.parametrize("query", list(LATERAL_OUTER_COLUMNS))
def test_lateral_outer_columns(query: str, dsn: str) -> None:
    assert answer(dsn, query) == LATERAL_OUTER_COLUMNS[query]
