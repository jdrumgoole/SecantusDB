"""A `char(n)[]` column's elements padded to the width (RP-719), and gap
tests of divergences found while working it.

PostgreSQL keeps every element of a `char(3)[]` value padded: an element
prints `ab `, `octet_length(c[1])` is 3, and a cast to a text array trims
each element (`rtrim1`). Its comparisons -- `=`, `<`, `@>`, `&&`, `IN`,
`array_position`, `array_remove`, `array_replace` -- use the element type's
`bpchareq` / `bpcharcmp`, which ignore trailing blanks. This server stores
the elements trimmed, as it does a scalar `char(n)`, pads them on output
and where they enter an expression, and compares them trimmed.

Found while working it (each a gap test below):

- `text[] && '{...}'` with an untyped literal was 42883 here, and `&&`
  missed a stored array whose lower bound is not 1 -- one cause: the
  containment operators took an array with its own lower bound (a tagged
  document) for a scalar (fixed, RP-720 and RP-721);
- GROUP BY over an array column returns its groups out of the ORDER BY's
  order;
- in a join's WHERE, an untyped array literal against an array column
  matched nothing, and so did a `char(n)` column against a padded
  `char(n)` constant (both fixed since: the narrow join path reads each
  WHERE constant as its column's type);
- GROUP BY a `char(n)` (or `char(n)[]`) value split a padded value from
  the same value unpadded (RP-725, since fixed: GROUP BY, DISTINCT,
  DISTINCT ON, an aggregate's DISTINCT and PARTITION BY now match a
  `char(n)` value without its trailing blanks).

Every expected answer was measured on a private, freshly initdb'd
PostgreSQL 15.19 cluster with `_SETUP`.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "drop table if exists t719, u719, x719, s719, s719b",
    "create table t719 (k int primary key, c char(3)[])",
    "insert into t719 values (1, '{ab}'), (2, array['cd', 'ab']), (3, '{\"ab  \"}'),"
    " (4, null), (5, '[0:1]={x,y}')",
    "create table u719 (k int primary key, c char(3)[])",
    "insert into u719 values (1, '{ab}')",
    "create table x719 (k int primary key, a text[])",
    "insert into x719 values (1, '{ab}'), (2, '{cd,ab}'), (5, '[0:1]={x,y}')",
    "create table s719 (k int primary key, c char(3))",
    "insert into s719 values (1, 'ab')",
    "create table s719b (k int primary key, c char(3))",
    "insert into s719b values (1, 'ab')",
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
    with psycopg.connect(dsn, autocommit=True) as c:
        try:
            cur = c.execute(query)
            return ("ok", cur.fetchall())
        except psycopg.Error as e:
            return ("error", e.sqlstate, str(e).splitlines()[0])


_AB, _CD_AB = (1, 3), (1, 2, 3)

PADDED = {
    "select k, c, octet_length(c[1]), c::text from t719 order by k": [
        (1, ["ab "], 3, '{"ab "}'),
        (2, ["cd ", "ab "], 3, '{"cd ","ab "}'),
        (3, ["ab "], 3, '{"ab "}'),
        (4, None, None, None),
        (5, ["x  ", "y  "], 3, '[0:1]={"x  ","y  "}'),
    ],
    "select k from t719 where c = '{ab}' order by k": [(k,) for k in _AB],
    "select k from t719 where c = '{ab}'::char(3)[] order by k": [(k,) for k in _AB],
    "select k from t719 where c <> '{ab}' order by k": [(2,), (5,)],
    "select k from t719 where c @> '{ab}'::char(3)[] order by k": [(k,) for k in _CD_AB],
    "select k from t719 where 'ab' = any(c) order by k": [(k,) for k in _CD_AB],
    "select k from t719 where c in ('{ab}'::char(3)[], '{x}') order by k": [(k,) for k in _AB],
    "select k, array_position(c, 'ab'), array_positions(c, 'ab') from t719 order by k": [
        (1, 1, [1]),
        (2, 2, [2]),
        (3, 1, [1]),
        (4, None, None),
        (5, None, []),
    ],
    "select k, array_remove(c, 'ab'), array_replace(c, 'ab', 'zz') from t719"
    " where k < 4 order by k": [(1, [], ["zz"]), (2, ["cd "], ["cd ", "zz"]), (3, [], ["zz"])],
    "select k, c = '{ab}', c < '{b}', c @> '{ab}' from t719 order by k": [
        (1, True, True, True),
        (2, False, False, True),
        (3, True, True, True),
        (4, None, None, None),
        (5, False, False, False),
    ],
    "select k, c::text[], c::varchar[] from t719 order by k": [
        (1, ["ab"], ["ab"]),
        (2, ["cd", "ab"], ["cd", "ab"]),
        (3, ["ab"], ["ab"]),
        (4, None, None),
        (5, ["x", "y"], ["x", "y"]),
    ],
    "select c[1], c[1] || '|', array_to_string(c, ',') || '|', to_json(c)::text"
    " from t719 where k = 2": [("cd ", "cd|", "cd ,ab |", '["cd ","ab "]')],
    "select t.k, t.c::text, u.c::text from t719 t join u719 u on t.k = u.k": [
        (1, '{"ab "}', '{"ab "}')
    ],
    "select '{ab}'::char(3)[] = '{ab}', '{ab}'::char(3)[] = '{ab}'::bpchar[],"
    " '{ab}'::char(3)[] @> '{ab}', '{ab}'::char(3)[] && '{ab}',"
    " '{ab}'::char(3)[] < '{ab,a}'::bpchar[]": [(True, True, True, True, True)],
    "select array_position('{ab}'::char(3)[], 'ab'), array_remove('{ab,cd}'::char(3)[], 'ab')": [
        (1, ["cd "])
    ],
}


@pytest.mark.backlog_fixed("RP-719")
@pytest.mark.parametrize("query", list(PADDED))
def test_a_char_n_array_is_padded_and_compared_without_its_blanks(query: str, dsn: str) -> None:
    assert _answer(dsn, query) == ("ok", PADDED[query])


@pytest.mark.backlog_fixed("RP-720")
@pytest.mark.parametrize(
    ("query", "rows"),
    [
        ("select k from x719 where a && '{ab,y}' order by k", [(1,), (2,), (5,)]),
        ("select k from x719 where a @> '{ab}' order by k", [(1,), (2,)]),
        ("select k from x719 where a <@ '{ab,cd}' order by k", [(1,), (2,)]),
    ],
)
def test_an_overlap_with_an_untyped_array_literal(query: str, rows: list, dsn: str) -> None:
    # `&&`, `@>` and `<@` resolve the untyped side to the column's `text[]`.
    # Row 5's `'[0:1]={x,y}'` was not taken for an array, so the literal
    # stayed text and the operator did not resolve (42883).
    assert _answer(dsn, query) == ("ok", rows)


@pytest.mark.backlog_fixed("RP-721")
def test_an_overlap_finds_an_array_with_its_own_lower_bound(dsn: str) -> None:
    # `arrayoverlap` compares elements; the bounds play no part. Row 5 was
    # missed, as were `@>` and `<@` over it.
    query = "select k from x719 where a && '{y}'::text[] order by k"
    assert _answer(dsn, query) == ("ok", [(5,)])


CONTAINMENT_OF_A_BOUNDED_ARRAY = {
    "select '[0:1]={x,y}'::text[] && '{y}'::text[], '[0:1]={x,y}'::text[] @> '{y}'::text[],"
    " '[0:1]={x,y}'::text[] <@ '{x,y,z}'::text[], '{y}'::text[] <@ '[0:1]={x,y}'::text[]": [
        (True, True, True, True)
    ],
    "select '[0:1][2:3]={{1,2},{3,4}}'::int[] @> '{4}',"
    " '[0:1]={x,NULL}'::text[] && '{NULL}'::text[]": [(True, False)],
    "select '[0:0]={ab}'::char(3)[] && '{ab}'::char(3)[], '[0:0]={ab}'::char(3)[] @> '{\"ab \"}'": [
        (True, True)
    ],
    "select array_fill(7, array[2], array[3]) && array[7],"
    " array[7] <@ array_fill(7, array[2], array[3])": [(True, True)],
    "select k from t719 where c @> '{y}' order by k": [(5,)],
    "select k from x719 where a <@ '{x,y,z}' order by k": [(5,)],
}


@pytest.mark.backlog_fixed("RP-721")
@pytest.mark.parametrize("query", list(CONTAINMENT_OF_A_BOUNDED_ARRAY))
def test_containment_reads_an_array_with_its_own_lower_bound(dsn: str, query: str) -> None:
    # Each was false (or 42883 beside an untyped literal) here; measured on
    # 15.19.
    assert _answer(dsn, query) == ("ok", CONTAINMENT_OF_A_BOUNDED_ARRAY[query])


@pytest.mark.backlog("RP-722")
def test_groups_of_an_array_column_follow_the_order_by(dsn: str) -> None:
    # `array_cmp` orders element by element: `{ab}` < `{cd,ab}` < `{x,y}`.
    query = "select count(*), a from x719 group by a order by 2"
    assert _answer(dsn, query) == ("ok", [(1, ["ab"]), (1, ["cd", "ab"]), (1, ["x", "y"])])


@pytest.mark.backlog_fixed("RP-723")
@pytest.mark.parametrize(
    "where",
    [
        "x.a = '{ab}'",
        "u.c = '{ab}'",
        "x.a = '{ab}' and u.c = '{ab}'",
        # Not only arrays: every untyped literal against a column stored as
        # something other than its text matched nothing (an int here).
        "x.k = '1'",
        "x.k < '2'",
    ],
)
def test_a_join_filter_reads_an_untyped_literal_as_the_array_column(where: str, dsn: str) -> None:
    # The narrow two-table join path compared the literal as the string it
    # was written; it is the column's type, as in a one-table filter.
    for join in ("join u719 u on x.k = u.k", "left join u719 u on x.k = u.k"):
        query = f"select x.k from x719 x {join} where {where}"
        assert _answer(dsn, query) == ("ok", [(1,)]), query
    if "u.c" not in where:
        query = f"select x.k from x719 x cross join (select 1 as k) u where {where}"
        assert _answer(dsn, query) == ("ok", [(1,)]), query


@pytest.mark.backlog_fixed("RP-724")
@pytest.mark.parametrize(
    ("where", "rows"),
    [
        ("s.c = 'ab '::char(3)", [(1,)]),
        ("s.c = 'ab '", [(1,)]),
        ("u.c = '{ab}'::char(3)[]", [(1,)]),
        # A `text` constant compares as text: the column's blanks are gone.
        ("s.c = 'ab '::text", []),
    ],
)
def test_a_join_filter_compares_a_char_n_column_without_its_blanks(
    where: str, rows: list, dsn: str
) -> None:
    # `bpchareq` ignores the constant's padding, which its cast added.
    if where.startswith("u."):
        query = f"select u.k from x719 x join u719 u on x.k = u.k where {where}"
    else:
        query = f"select s.k from s719 s join s719b b on s.k = b.k where {where}"
    assert _answer(dsn, query) == ("ok", rows)


@pytest.mark.backlog_fixed("RP-725")
@pytest.mark.parametrize(
    "values",
    ["('ab'::char(3)), ('ab'::bpchar)", "('{ab}'::char(3)[]), ('{ab}'::bpchar[])"],
)
def test_grouping_a_char_n_value_ignores_its_blanks(values: str, dsn: str) -> None:
    # `bpchar`'s hash and equality ignore trailing blanks: one group of two,
    # where this server made two groups of one.
    query = f"select count(*) from (values {values}) v(a) group by a"
    assert _answer(dsn, query) == ("ok", [(2,)])


_PAD, _BARE = "('ab'::char(3)), ('ab'::bpchar)", "('ab'::bpchar), ('ab'::char(3))"
_ARR = "('{ab}'::char(3)[]), ('{ab}'::bpchar[])"

# Every way a query matches values by equality, over a padded `char(n)` and
# the same value unpadded (the VALUES column is a `bpchar` with no width, so
# each keeps its own blanks). The value a group or a DISTINCT prints is its
# first row's, blanks and all.
BLANK_INSENSITIVE = {
    f"select a, octet_length(a), count(*) from (values {_PAD}) v(a) group by a": [("ab ", 3, 2)],
    f"select a, octet_length(a), count(*) from (values {_BARE}) v(a) group by a": [("ab", 2, 2)],
    f"select count(distinct a) from (values {_PAD}) v(a)": [(1,)],
    f"select array_agg(distinct a) from (values {_PAD}) v(a)": [(["ab "],)],
    f"select distinct a from (values {_PAD}) v(a)": [("ab ",)],
    f"select distinct on (a) a, octet_length(a) from (values {_PAD}) v(a)": [("ab ", 3)],
    f"select count(*) over (partition by a) from (values {_PAD}) v(a)": [(2,), (2,)],
    f"select count(distinct a) from (values {_ARR}) v(a)": [(1,)],
    f"select distinct a from (values {_ARR}) v(a)": [(["ab "],)],
    # A `text` or `varchar` keeps its blanks: two groups.
    "select count(*) from (values ('ab '::text), ('ab'::text)) v(a) group by a": [(1,), (1,)],
    "select count(*) from (values ('ab '::varchar), ('ab'::varchar)) v(a) group by a": [
        (1,),
        (1,),
    ],
}


@pytest.mark.backlog_fixed("RP-725")
@pytest.mark.parametrize("query", list(BLANK_INSENSITIVE))
def test_distinct_and_partitions_match_a_char_n_value_without_its_blanks(
    query: str, dsn: str
) -> None:
    assert _answer(dsn, query) == ("ok", BLANK_INSENSITIVE[query])
