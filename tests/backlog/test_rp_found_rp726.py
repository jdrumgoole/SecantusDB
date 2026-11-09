"""What working RP-726 (a `varchar` compared with a `char(n)`) fixed, and
what it found:

- every form of the comparison the expression evaluator answers now reads
  the `varchar` side as `bpchar` (the regression test below);
- a JOIN ... ON a `varchar` column equal to a `char(n)` one misses the row
  whose `varchar` has trailing blanks;
- a simple CASE and NULLIF over a `char(n)` and another string type
  compare without the operator their types resolve to, so the `char(n)`
  operand keeps its blanks.

An answer is the rows and the column names. Measured on a private, freshly
initdb'd PostgreSQL 15.19 cluster with `_SETUP`.
"""

from __future__ import annotations

from collections.abc import Iterator
from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "drop table if exists ch726, d726t cascade",
    "drop domain if exists dv726",
    "create table ch726 (k int, c char(3), v varchar(4), t text)",
    "insert into ch726 values (1, 'y', 'y', 'y'), (2, 'ab', 'ab ', 'ab ')",
    "create domain dv726 as varchar(5)",
    "create table d726t (k int, d dv726)",
    "insert into d726t values (1, 'ab ')",
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
        c.execute(_SETUP[1])


def answer(dsn: str, query: str) -> Any:
    with psycopg.connect(dsn, autocommit=True) as c:
        cur = c.execute(query)
        return (cur.fetchall(), [d.name for d in cur.description or []])


# `varchar op bpchar` resolves to the `bpchar` operator, the `varchar` side
# cast to `bpchar` (an implicit cast), so neither side's trailing blanks
# count: the ordering operators, a WHERE, IN, a row constructor's pair, a
# domain over `varchar`. `text = bpchar` is `texteq` instead (the `char(n)`
# side cast to text), where the text side keeps its blanks, and `||` is
# `textcat`. Each form diverged before the fix but the `text` ones.
VARCHAR_AGAINST_CHAR_N = {
    "select k, v <> c, v < c, v > c, v <= c, c >= v from ch726 order by k": (
        [(1, False, False, False, True, True), (2, False, False, False, True, True)],
        ["k", "?column?", "?column?", "?column?", "?column?", "?column?"],
    ),
    "select k from ch726 where v = c order by k": ([(1,), (2,)], ["k"]),
    "select k from ch726 where c = v order by k": ([(1,), (2,)], ["k"]),
    "select k, (v, k) = (c, 2), (c, k) = (v, 2) from ch726 order by k": (
        [(1, False, False), (2, True, True)],
        ["k", "?column?", "?column?"],
    ),
    "select k, v in (c, 'zz'::char(2)) from ch726 order by k": (
        [(1, True), (2, True)],
        ["k", "?column?"],
    ),
    "select k, t = c, v || c from ch726 order by k": (
        [(1, True, "yy"), (2, False, "ab ab")],
        ["k", "?column?", "?column?"],
    ),
    "select k, d = 'ab'::char(3), 'ab'::char(3) = d from d726t": (
        [(1, True, True)],
        ["k", "?column?", "?column?"],
    ),
    "select 'ab '::varchar = 'ab'::char(3), 'ab'::char(3) = 'ab  '::varchar,"
    " 'a  '::varchar < 'a'::char(2)": (
        [(True, True, False)],
        ["?column?", "?column?", "?column?"],
    ),
    "select x.k, y.k from ch726 x, ch726 y where x.v = y.c order by 1, 2": (
        [(1, 1), (2, 2)],
        ["k", "k"],
    ),
}


@pytest.mark.backlog_fixed("RP-726")
@pytest.mark.parametrize("query", list(VARCHAR_AGAINST_CHAR_N))
def test_varchar_against_char_n_every_form(query: str, dsn: str) -> None:
    assert answer(dsn, query) == VARCHAR_AGAINST_CHAR_N[query]


# `x.v = y.c` is `bpchareq` over `x.v::bpchar`, and the hash join hashes it
# with `hashbpchar`: both ignore trailing blanks, so `'ab '` meets `'ab'`.
# Here the bare equality is a hash-join key compared as the stored values.
JOIN_ON_VARCHAR_AGAINST_CHAR_N = {
    "select x.k, y.k from ch726 x join ch726 y on x.v = y.c order by 1, 2": (
        [(1, 1), (2, 2)],
        ["k", "k"],
    ),
    "select x.k, y.k from ch726 x join ch726 y on y.c = x.v order by 1, 2": (
        [(1, 1), (2, 2)],
        ["k", "k"],
    ),
    "select x.k, y.k from ch726 x left join ch726 y on x.v = y.c order by 1, 2": (
        [(1, 1), (2, 2)],
        ["k", "k"],
    ),
}


@pytest.mark.backlog("RP-731")
@pytest.mark.parametrize("query", list(JOIN_ON_VARCHAR_AGAINST_CHAR_N))
def test_join_on_varchar_against_char_n(query: str, dsn: str) -> None:
    assert answer(dsn, query) == JOIN_ON_VARCHAR_AGAINST_CHAR_N[query]


# A simple CASE is `test = when-value` and NULLIF is `a = b`, each through
# the operator the two types resolve to: `bpchareq` for a `varchar` against
# a `char(n)`, `texteq` against `text` with the `char(n)` side cast to text
# (its blanks dropped). Here the `char(n)` side is compared padded. Two
# `char(n)` operands are answered (`case c when 'y'::char(3)`).
CASE_AND_NULLIF = {
    "select k, case c when v then 'eq' else 'ne' end from ch726 order by k": (
        [(1, "eq"), (2, "eq")],
        ["k", "case"],
    ),
    "select k, case t when c then 'eq' else 'ne' end from ch726 order by k": (
        [(1, "eq"), (2, "ne")],
        ["k", "case"],
    ),
    "select k, nullif(v, c) from ch726 order by k": (
        [(1, None), (2, None)],
        ["k", "nullif"],
    ),
    "select k, nullif(t, c) from ch726 order by k": (
        [(1, None), (2, "ab ")],
        ["k", "nullif"],
    ),
}


@pytest.mark.backlog("RP-732")
@pytest.mark.parametrize("query", list(CASE_AND_NULLIF))
def test_case_and_nullif_compare_by_type(query: str, dsn: str) -> None:
    assert answer(dsn, query) == CASE_AND_NULLIF[query]
