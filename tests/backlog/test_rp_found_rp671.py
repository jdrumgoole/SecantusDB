"""What working RP-671 (`min` / `max` over a `char(n)` column keep the
value's blanks) pinned and found:

- RP-671 (fixed): the padded answer still compares as `bpchar` -- in a
  HAVING, an IN list, an IN subquery, a join and a set operation -- because
  this server keeps a `char(n)` value trimmed and pads it only where it
  enters an expression. (Padding the aggregate's stored answer instead got
  every one of these wrong.)
- RP-679 (fixed since): a `char(n)` value that carried its blanks (a cast to
  `char(n)`, a scalar subquery over a `char(n)` column) compared and sorted
  with them;
- RP-680 (fixed): a HAVING comparison of a `char(n)` `min` / `max` against
  a `char(n)` value, an `= ANY` array or `IS DISTINCT FROM` did not compare
  as `bpchar`, and `IS DISTINCT FROM`, `= ANY` and `<> ALL` over any grouped
  value were read as a plain `=` / `<>`;
- RP-681 (fixed): `min` / `max` over a `varchar` column were typed
  `varchar`, not `text`;
- RP-682 (fixed): OVER on a function that is not a window function or an
  aggregate was 0A000, not 42809.

Measured on PostgreSQL 15.19 (the shared reference on port 5415): the
VALUES-only statements as written, the rest with `_SETUP`'s tables made
temporary.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "drop table if exists t671, t671s cascade",
    "create table t671 (k int, f char(3))",
    "insert into t671 values (1, 'a'), (2, 'xy'), (3, 'b')",
    "create table t671s (k int, f char(3))",
    "insert into t671s values (1, 'a'), (2, E'a\\t'), (3, 'b')",
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


# Each of these answered as PostgreSQL does before RP-671 and wrongly when
# the aggregate's grouped value itself was padded.
STILL_BPCHAR = {
    "select k from t671 group by k having max(f) in ('a', 'xy') order by k": [(1,), (2,)],
    "select k from t671 group by k having max(f) between 'a' and 'b' order by k": [(1,), (3,)],
    "select k from t671 group by k having max(f) not in ('a') order by k": [(2,), (3,)],
    "select k from (select k, max(f) x from t671 group by k) s where x in ('a', 'xy') order by k": [
        (1,),
        (2,),
    ],
    "select k from (select k, max(f) x from t671 group by k) s where x = 'xy' order by k": [(2,)],
    "select k from (select k, max(f) x from t671 group by k) s where x > 'a' order by k": [
        (2,),
        (3,),
    ],
    "select k from t671 where f in (select max(f) from t671 group by k) order by k": [
        (1,),
        (2,),
        (3,),
    ],
    "select count(*) from (select max(f) m from t671 group by k) s join t671 on s.m = t671.f": [
        (3,)
    ],
    "select count(*) from (select max(f) m from t671 group by k union select f from t671) s": [
        (3,)
    ],
    "select count(*) from (select max(f) m from t671 group by k intersect select f from t671) s": [
        (3,)
    ],
    "select count(distinct m) from (select max(f) m from t671 group by k"
    " union all select f from t671) s": [(3,)],
    "select k, min(f) from t671s group by k order by min(f)": [
        (1, "a  "),
        (2, "a\t "),
        (3, "b  "),
    ],
    "select k, min(f) from t671s group by k order by min(f) desc": [
        (3, "b  "),
        (2, "a\t "),
        (1, "a  "),
    ],
}


@pytest.mark.backlog_fixed("RP-671")
@pytest.mark.parametrize("query", list(STILL_BPCHAR))
def test_min_max_over_char_n_still_compare_as_bpchar(query: str, dsn: str) -> None:
    assert _rows(dsn, query) == STILL_BPCHAR[query]


# `bpchareq` / `bpcharcmp` drop the trailing blanks first (`bcTruelen`):
# `'xy   '::char(5) = 'xy'` is true, and `'a  '` sorts before `'a\t '`.
# Here a value that carried its blanks was compared with them, until RP-679
# stored a FROM-subquery's `char(n)` cell trimmed and trimmed a `char(n)`
# sort key.
PADDED = {
    "select k from (select k, f::char(5) x from t671) s where x in ('a', 'xy') order by k": [
        (1,),
        (2,),
    ],
    "select k from (select k, f::char(5) x from t671) s where x = 'xy' order by k": [(2,)],
    "select k from (select k, (select f) x from t671) s where x in ('a', 'xy') order by k": [
        (1,),
        (2,),
    ],
    "select k from t671s order by f::char(5)": [(1,), (2,), (3,)],
    "select k from (select k, f::char(5) x from t671s) s order by x": [(1,), (2,), (3,)],
    "select k from t671s order by (select f)": [(1,), (2,), (3,)],
}


@pytest.mark.backlog_fixed("RP-679")
@pytest.mark.parametrize("query", list(PADDED))
def test_padded_char_n_compares_without_its_blanks(query: str, dsn: str) -> None:
    assert _rows(dsn, query) == PADDED[query]


HAVING = {
    "select k from t671 group by k having max(f) = any('{a,xy}') order by k": [(1,), (2,)],
    "select k from t671 group by k having max(f) is distinct from 'xy' order by k": [
        (1,),
        (3,),
    ],
    "select k from t671 group by k having max(f) = 'xy'::char(3) order by k": [(2,)],
    "select k from t671 group by k having max(f) = 'xy  '::char(5) order by k": [(2,)],
    "select k, max(f) from t671 group by k having max(f) = 'xy  ' order by k": [(2, "xy ")],
}


@pytest.mark.backlog_fixed("RP-680")
@pytest.mark.parametrize("query", list(HAVING))
def test_having_over_char_n_min_max_compares_as_bpchar(query: str, dsn: str) -> None:
    assert _rows(dsn, query) == HAVING[query]


# What fixing RP-680 measured besides its own five: the HAVING shortcut read
# `IS DISTINCT FROM`, `= ANY` and `<> ALL` as a plain `=` / `<>` over every
# type, and compared a grouped `char(n)` key with a constant's blanks.
# PostgreSQL 15.19, on a private cluster and the shared one alike.
HAVING_SHAPES = {
    "select k from t671 group by k having max(k) is distinct from 2 order by k": [(1,), (3,)],
    "select k from t671 group by k having max(k) is distinct from null order by k": [
        (1,),
        (2,),
        (3,),
    ],
    "select k from t671 group by k having max(k) is not distinct from 2 order by k": [(2,)],
    "select k from t671 group by k having max(k) = any('{1,2}') order by k": [(1,), (2,)],
    "select k from t671 group by k having max(f) <> all('{a,xy}') order by k": [(3,)],
    "select k from t671 group by k having 'xy  ' = max(f) order by k": [(2,)],
    "select k from t671 group by k having max(f) = 'xy  '::varchar order by k": [(2,)],
    "select k from t671 group by k having max(f) = 'xy' order by k": [(2,)],
    "select k from t671 group by k having max(f) > 'a  ' order by k": [(2,), (3,)],
    "select f from t671 group by f having f = 'xy  ' order by f": [("xy ",)],
    "select f from t671 group by f having f = 'xy  '::char(5) order by f": [("xy ",)],
    "select f from t671 group by f having f is distinct from 'xy' order by f": [
        ("a  ",),
        ("b  ",),
    ],
    "select f from t671 group by f having f = any('{a,xy}') order by f": [
        ("a  ",),
        ("xy ",),
    ],
}


@pytest.mark.backlog_fixed("RP-680")
@pytest.mark.parametrize("query", list(HAVING_SHAPES))
def test_having_reads_each_expression_kind(query: str, dsn: str) -> None:
    assert _rows(dsn, query) == HAVING_SHAPES[query]


_TEXT = 25


@pytest.mark.backlog_fixed("RP-681")
@pytest.mark.parametrize("func", ["max", "min"])
def test_min_max_over_varchar_is_text(func: str, dsn: str) -> None:
    """PostgreSQL has no `max(varchar)`: the call resolves to `max(text)`
    and was described `text` on 15.19, and `varchar` here. The wider shapes
    are in `test_rp_found_rp681.py`."""
    query = f"select {func}(v) from (values ('ab'::varchar(4)), ('cd')) s(v)"
    with psycopg.connect(dsn, autocommit=True) as c:
        cur = c.execute(query)
        assert cur.description is not None
        assert cur.description[0].type_code == _TEXT


OVER = {
    "select octet_length('x') over ()": "octet_length",
    "select upper(v) over () from (values ('ab'::text)) s(v)": "upper",
    "select length(v) over (order by v) from (values ('ab'::text)) s(v)": "length",
}


@pytest.mark.backlog_fixed("RP-682")
@pytest.mark.parametrize("query", list(OVER))
def test_over_on_a_plain_function_is_42809(query: str, dsn: str) -> None:
    """Was 0A000 `window function octet_length() is not supported yet`."""
    name = OVER[query]
    assert _rows(dsn, query) == (
        "error",
        "42809",
        f"OVER specified, but {name} is not a window function nor an aggregate function",
    )


# `ParseFuncOrColumn`'s order, measured on PostgreSQL 15.19: the arguments
# and the function resolve first (42883), then the first aggregate-only
# clause a plain function carries is named, the function as written.
OVER_ORDER = {
    "select upper(1) over ()": ("error", "42883", "function upper(integer) does not exist"),
    "select nosuch(1) over ()": ("error", "42883", "function nosuch(integer) does not exist"),
    "select upper(nosuch3()) over ()": ("error", "42883", "function nosuch3() does not exist"),
    "select pg_catalog.upper('a') over ()": (
        "error",
        "42809",
        "OVER specified, but pg_catalog.upper is not a window function nor an aggregate function",
    ),
    "select 1 + upper('a') over ()": (
        "error",
        "42809",
        "OVER specified, but upper is not a window function nor an aggregate function",
    ),
    "select now(*) over ()": (
        "error",
        "42809",
        "now(*) specified, but now is not an aggregate function",
    ),
    "select upper('a') filter (where true) over ()": (
        "error",
        "42809",
        "FILTER specified, but upper is not an aggregate function",
    ),
    "select upper(distinct 'a') filter (where true) over ()": (
        "error",
        "42809",
        "DISTINCT specified, but upper is not an aggregate function",
    ),
}


@pytest.mark.backlog_fixed("RP-682")
@pytest.mark.parametrize("query", list(OVER_ORDER))
def test_over_on_a_plain_function_resolves_the_call_first(query: str, dsn: str) -> None:
    """Each was 0A000 `window function ... is not supported yet`."""
    assert _rows(dsn, query) == OVER_ORDER[query]


@pytest.mark.backlog_fixed("RP-682")
def test_over_on_a_plain_function_calls_nothing(dsn: str) -> None:
    """The refusal is the parser's: `nextval` under OVER never ran, so the
    sequence's first value is still 1 (PostgreSQL 15.19)."""
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute("drop sequence if exists s682")
        c.execute("create sequence s682")
        try:
            with pytest.raises(psycopg.Error) as e:
                c.execute("select nextval('s682') over ()")
            assert e.value.sqlstate == "42809"
            assert c.execute("select nextval('s682')").fetchall() == [(1,)]
        finally:
            c.execute("drop sequence s682")
