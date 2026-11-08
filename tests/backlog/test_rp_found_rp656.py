"""The `ARRAY(select ...)` sublink typed at its column's modifier (RP-656),
and the gap tests of what working it found:

- RP-656 (fixed): an `ARRAY(select c ...)` over a `char(n)` column, or a
  `char(n)[]` one, keeps each element's blanks and describes the column's
  modifier, as the array of its column's type at that modifier;
- RP-666 (fixed): a scalar subquery over a `char(n)` column, or a
  `char(n)[]` one, keeps the value's blanks, and an empty one describes
  the column's modifier;
- an `ARRAY(select n ...)` over a `numeric(p,s)` column describes no
  modifier;
- `min` / `max` over a `char(n)` column drop the value's blanks (RP-671).

The expected answers were measured on a private, freshly initdb'd
PostgreSQL 15.19 cluster with `_SETUP`, which each statement runs after;
RP-666's wider cases and RP-671's on the shared PostgreSQL 15.19 reference
(port 5415), with `_SETUP`'s table made temporary.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "drop table if exists t656 cascade",
    "create table t656 (c char(3), v varchar(4), n numeric(5,2), ca char(3)[])",
    "insert into t656 values ('xy', 'ab', 2.5, '{a,bc}')",
]


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        c.execute(_SETUP[0])


def _answer(dsn: str, query: str) -> tuple[object, ...]:
    """The first column's type OID and modifier, and the rows."""
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP:
            c.execute(q)
        try:
            cur = c.execute(query)
        except psycopg.Error as e:
            return ("error", e.sqlstate, str(e).splitlines()[0])
        assert cur.description is not None and cur.pgresult is not None
        return (cur.description[0].type_code, cur.pgresult.fmod(0), cur.fetchall())


_BOOL = 16
_INT4 = 23
_TEXT = 25
_BPCHAR = 1042
_VARCHAR = 1043
_BPCHAR_ARRAY = 1014
_VARCHAR_ARRAY = 1015
_NUMERIC_ARRAY = 1231

# The sublink is the array of its column's type at the column's modifier
# (`exprTypmod` of an ARRAY sublink is its column's), so a `char(3)` value
# keeps its blanks in it, element by element in a `char(3)[]` column's.
ARRAY_SUBLINKS = {
    "select array(select c from t656)": (_BPCHAR_ARRAY, 7, [(["xy "],)]),
    "select array(select ca from t656)": (_BPCHAR_ARRAY, 7, [([["a  ", "bc "]],)]),
    "select array(select c from t656 where false)": (_BPCHAR_ARRAY, 7, [([],)]),
    "select array(select c::char(5) from t656)": (_BPCHAR_ARRAY, 9, [(["xy   "],)]),
    "select array(select v from t656)": (_VARCHAR_ARRAY, 8, [(["ab"],)]),
    "select array(select c from t656) = array['xy ']::char(3)[]": (_BOOL, -1, [(True,)]),
    "select octet_length((array(select c from t656))[1])": (_INT4, -1, [(3,)]),
}


@pytest.mark.backlog_fixed("RP-656")
@pytest.mark.parametrize("query", list(ARRAY_SUBLINKS))
def test_array_sublink_is_typed_at_its_columns_modifier(query: str, dsn: str) -> None:
    assert _answer(dsn, query) == ARRAY_SUBLINKS[query]


# A scalar subquery's value is its column's value at the column's modifier
# (`exprTypmod` of an EXPR sublink is its column's): a `char(3)` one keeps
# its blanks, element by element in a `char(3)[]` column's, and an empty
# one is a NULL described at that modifier.
SCALAR_SUBLINKS = {
    "select (select c from t656)": (_BPCHAR, 7, [("xy ",)]),
    "select octet_length((select c from t656))": (_INT4, -1, [(3,)]),
    "select (select c::char(5) from t656)": (_BPCHAR, 9, [("xy   ",)]),
    "select (select c from t656) || 'z'": (_TEXT, -1, [("xyz",)]),
    "select length((select c from t656))": (_INT4, -1, [(2,)]),
    "select (select c from t656 where false)": (_BPCHAR, 7, [(None,)]),
    "select (select v from t656)": (_VARCHAR, 8, [("ab",)]),
    "select (select ca from t656)": (_BPCHAR_ARRAY, 7, [(["a  ", "bc "],)]),
    "select octet_length((select ca from t656)[1])": (_INT4, -1, [(3,)]),
}


@pytest.mark.backlog_fixed("RP-666")
@pytest.mark.parametrize("query", list(SCALAR_SUBLINKS))
def test_scalar_subquery_keeps_char_padding(query: str, dsn: str) -> None:
    assert _answer(dsn, query) == SCALAR_SUBLINKS[query]


# `exprTypmod` of an ARRAY sublink is its column's, whatever the type: a
# `numeric(5,2)` column's array describes 327686 ((5 << 16 | 2) + 4).
@pytest.mark.backlog("RP-667")
def test_array_sublink_of_numeric_describes_its_modifier(dsn: str) -> None:
    got = _answer(dsn, "select array(select n from t656)")
    assert got[:2] == (_NUMERIC_ARRAY, 327686)


# An Aggref is its argument's type, and `min` / `max` return one of the
# stored values, blanks and all: `octet_length(max(c))` is 3. Here the
# value is the stored one without its padding, which a RowDescription at
# the column's width (RP-659) puts back in print only.
MIN_MAX = {
    "select octet_length(max(c)) from t656": [(3,)],
    "select octet_length(min(c)) from t656": [(3,)],
    "select octet_length((select max(c) from t656))": [(3,)],
}


@pytest.mark.backlog("RP-671")
@pytest.mark.parametrize("query", list(MIN_MAX))
def test_min_max_over_char_n_keep_its_blanks(query: str, dsn: str) -> None:
    assert _answer(dsn, query)[2:] == (MIN_MAX[query],)
