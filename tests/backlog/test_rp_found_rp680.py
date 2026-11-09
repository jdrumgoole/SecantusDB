"""What working RP-680 (a HAVING comparison of a `char(n)` `min` / `max`
compares as `bpchar`) found:

- RP-685: a `char(n)` column compared with a `text` value compared as
  `bpchar`, dropping the constant's blanks, where PostgreSQL casts the
  column to `text` and compares `text`s (fixed);
- RP-684 (fixed): `= ANY` of a `char(n)` array found nothing over a
  `char(n)` column, as the array's padded elements were compared with their
  blanks.

Each is wrong in a plain WHERE, in a FROM-subquery's column over a `min` /
`max` and in a HAVING alike. Measured on a private PostgreSQL 15.19 cluster
with `_SETUP`, and on the shared one with the table temporary: the same rows.

What working RP-685 found:

- RP-691: an IN list or a BETWEEN over a `char(n)` column compared with the
  constants' blanks, so `in ('xy  ')` found nothing (fixed).
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "drop table if exists t680 cascade",
    "create table t680 (k int, f char(3))",
    "insert into t680 values (1, 'a'), (2, 'xy'), (3, 'b')",
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


def _rows(dsn: str, query: str) -> object:
    """The rows, or the error's SQLSTATE and first line."""
    with psycopg.connect(dsn, autocommit=True) as c:
        try:
            return c.execute(query).fetchall()
        except psycopg.Error as e:
            return ("error", e.sqlstate, str(e).splitlines()[0])


_GROUPED = "(select k, max(f) x from t680 group by k) s"

# No `bpchar = text` operator: the column is cast to `text` (`rtrim`), so
# the constant's blanks count.
TEXT = {
    "select k from t680 where f = 'xy  '::text order by k": [],
    "select k from t680 where f = 'xy'::text order by k": [(2,)],
    f"select k from {_GROUPED} where x = 'xy  '::text order by k": [],
    "select k from t680 group by k having max(f) = 'xy  '::text order by k": [],
    # `name` compares as `text` too; a `varchar` is cast to `bpchar`.
    "select k from t680 where f = 'xy  '::name order by k": [],
    "select k from t680 where f = 'xy  '::varchar order by k": [(2,)],
    # The column is `rtrim`med, the constant kept: `'xy' < 'xy '`.
    "select k from t680 where f < 'xy '::text order by k": [(1,), (2,), (3,)],
    "select k from t680 where f <> 'xy  '::text order by k": [(1,), (2,), (3,)],
    "select k from t680 where f = 'xy' || ' ' order by k": [],
    "select k from t680 where f = rpad('xy', 4) order by k": [],
    "select k from t680 where k = 1 or f = 'xy  '::text order by k": [(1,)],
    # An untyped literal is `bpchar`: its blanks are dropped.
    "select k from t680 where f = 'xy  ' order by k": [(2,)],
}


@pytest.mark.backlog_fixed("RP-685")
@pytest.mark.parametrize("query", list(TEXT))
def test_char_n_against_text_compares_as_text(query: str, dsn: str) -> None:
    assert _rows(dsn, query) == TEXT[query]


# `bpchareq` drops the trailing blanks of both sides (`bcTruelen`); a
# `char(n)` side meets a `text[]` as `text` through `rtrim`, so the text
# elements keep theirs. Each measured on 15.19.
ANY = {
    """select k from t680 where f = any('{"a ",xy}'::text[]) order by k""": [(2,)],
    "select k from t680 where f = any(array['a ']) order by k": [],
    """select k from t680 where f = any('{"a ",xy}') order by k""": [(1,), (2,)],
    """select k from t680 where f <> all('{"a  ",b}'::char(3)[]) order by k""": [(2,)],
    """select k from t680 where f < all('{"b ",xy}'::char(3)[]) order by k""": [(1,)],
    """select k from t680 where (f || '') = any('{"a  "}'::char(3)[]) order by k""": [(1,)],
    "select k from t680 where f = any('{a,xy}'::char(3)[]) order by k": [(1,), (2,)],
    f"select k from {_GROUPED} where x = any('{{a,xy}}'::char(3)[]) order by k": [
        (1,),
        (2,),
    ],
    "select k from t680 group by k having max(f) = any('{a,xy}'::char(3)[]) order by k": [
        (1,),
        (2,),
    ],
}


@pytest.mark.backlog_fixed("RP-684")
@pytest.mark.parametrize("query", list(ANY))
def test_any_of_a_char_n_array_compares_as_bpchar(query: str, dsn: str) -> None:
    """An array's padded `char(n)` elements were compared with their blanks
    against the trimmed stored value, and found nothing."""
    assert _rows(dsn, query) == ANY[query]


# An IN list's members take the column's type (`select_common_type` keeps
# `bpchar`, even beside a `text` member); BETWEEN is two operators, each
# resolved on its own.
IN_BETWEEN = {
    "select k from t680 where f in ('xy  ', 'q') order by k": [(2,)],
    "select k from t680 where f in ('xy  ') order by k": [(2,)],
    "select k from t680 where f in ('xy  '::text, 'q'::text) order by k": [(2,)],
    "select k from t680 where f not in ('xy  ', 'q') order by k": [(1,), (3,)],
    "select k from t680 where f between 'xy  ' and 'xy  ' order by k": [(2,)],
    "select k from t680 where f between 'xy '::char(3) and 'xy'::char(3) order by k": [(2,)],
    "select k from t680 where f between 'xy '::text and 'xz'::text order by k": [],
}


@pytest.mark.backlog_fixed("RP-691")
@pytest.mark.parametrize("query", list(IN_BETWEEN))
def test_in_and_between_over_char_n_compare_as_bpchar(query: str, dsn: str) -> None:
    assert _rows(dsn, query) == IN_BETWEEN[query]
