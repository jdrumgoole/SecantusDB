"""What working RP-697 (a row compared with a correlated subquery) found:

- a `varchar` compared with a `char(n)` counts the trailing blanks;
- an uncorrelated `row op ANY / ALL (subquery)` with an ordering operator
  is refused;
- a row compared with a subquery whose column types have no operator
  together is refused at run time, not reported as 42883 when planned.

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
    "drop table if exists rn697, ch697 cascade",
    "create table rn697 (k int, a int, c text)",
    "insert into rn697 values (1, 1, 'y'), (2, null, 'y'), (3, 2, 'z'), (4, 1, null)",
    "create table ch697 (k int, c char(3), v varchar(4))",
    "insert into ch697 values (1, 'y', 'y'), (2, 'ab', 'ab ')",
]


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    url = _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(url, autocommit=True) as c:
        for q in _SETUP:
            c.execute(q)
    yield url
    with psycopg.connect(url, autocommit=True) as c:
        c.execute(_SETUP[0])


def answer(dsn: str, query: str) -> Any:
    with psycopg.connect(dsn, autocommit=True) as c:
        try:
            cur = c.execute(query)
        except psycopg.Error as e:
            return (e.sqlstate, e.diag.message_primary, e.diag.statement_position)
        return (cur.fetchall(), [d.name for d in cur.description or []])


# `varchar = bpchar` resolves to `bpchareq` (the `varchar` side cast to
# `bpchar`), which ignores trailing blanks on both sides: `'ab '::varchar`
# equals `'ab'::char(3)`. Here row 2 is false both ways round, and a row
# comparison whose pair is a `varchar` and a `char(n)` inherits it.
VARCHAR_AGAINST_BPCHAR = {
    "select k, v = c, c = v from ch697 order by k": (
        [(1, True, True), (2, True, True)],
        ["k", "?column?", "?column?"],
    ),
    "select k, (v, k) = (select x.c, x.k from ch697 x where x.k = 2) from ch697 order by k": (
        [(1, False), (2, True)],
        ["k", "?column?"],
    ),
}


@pytest.mark.backlog("RP-726")
@pytest.mark.parametrize("query", list(VARCHAR_AGAINST_BPCHAR))
def test_varchar_against_bpchar_ignores_trailing_blanks(query: str, dsn: str) -> None:
    assert answer(dsn, query) == VARCHAR_AGAINST_BPCHAR[query]


# `(k, a) < any (select 2, 3)`: each subquery row is a RowCompareExpr with
# the row. Here it is 0A000 `a row comparison with < and a subquery is not
# supported yet`; only `=` and `<>` are answered uncorrelated.
UNCORRELATED_ORDERED_ANY = {
    "select k, (k, a) < any (select 2, 3) from rn697 order by k": (
        [(1, True), (2, None), (3, False), (4, False)],
        ["k", "?column?"],
    ),
    "select k, (k, a) >= all (select 2, 1 union all select 1, 5) from rn697 order by k": (
        [(1, False), (2, None), (3, True), (4, True)],
        ["k", "?column?"],
    ),
}


@pytest.mark.backlog("RP-727")
@pytest.mark.parametrize("query", list(UNCORRELATED_ORDERED_ANY))
def test_uncorrelated_ordered_row_any(query: str, dsn: str) -> None:
    assert answer(dsn, query) == UNCORRELATED_ORDERED_ANY[query]


# `make_row_comparison_op` resolves each pair's operator as the statement
# is analysed: `c = k`, text against integer, is 42883 at the row's
# operator, even when no row is ever compared. Here it is 0A000 `comparing
# string with int32 using = is not supported yet` with no position, and no
# error at all over no rows.
ROW_PAIR_WITH_NO_OPERATOR = {
    "select k, (a, c) = (select 1, 2) from rn697 order by k": (
        "42883",
        "operator does not exist: text = integer",
        "18",
    ),
    "select k, (a, c) in (select rn697.a, rn697.k) from rn697 order by k": (
        "42883",
        "operator does not exist: text = integer",
        "18",
    ),
    "select k, (a, c) = (select rn697.a, rn697.k) from rn697 where false": (
        "42883",
        "operator does not exist: text = integer",
        "18",
    ),
}


@pytest.mark.backlog("RP-728")
@pytest.mark.parametrize("query", list(ROW_PAIR_WITH_NO_OPERATOR))
def test_row_pair_with_no_operator_is_42883(query: str, dsn: str) -> None:
    assert answer(dsn, query) == ROW_PAIR_WITH_NO_OPERATOR[query]
