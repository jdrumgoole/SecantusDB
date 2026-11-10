"""`min` / `max` take the type of the overload they resolve to (RP-681),
and what was found while working it.

PostgreSQL 15 has 22 `min` / `max` overloads (`pg_aggregate`), and a call
over any other type resolves like any function call
(`func_select_candidate`): `varchar`, `name` and `"char"` reach `max(text)`
by an implicit cast, so the answer is `text`. This server described the
input type instead, in the RowDescription, `pg_typeof`, a FROM-subquery or
CTE column, a window call and a view's column.

A `reg*` reaches `max(oid)` and `cidr` reaches `max(inet)` the same way,
and there each input is coerced before it is compared: `max(regclass)` is
the largest OID, as an `oid`. Here they kept the input type (RP-706, fixed).
And a `text` scalar subquery inside `pg_typeof` was refused (RP-707, fixed).

The expected answers were measured on a private, freshly initdb'd
PostgreSQL 15.19 cluster with `_SETUP`.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "drop view if exists v681",
    "drop table if exists t681 cascade",
    "drop domain if exists dv681",
    "create domain dv681 as varchar(5)",
    'create table t681 (k int, v varchar(4), w varchar, n name, q "char", e dv681)',
    "insert into t681 values (1, 'ab', 'x', 'nm', 'a', 'ee'), (1, 'cd', 'y', 'nn', 'b', 'ef')",
]

_TEXT = 25
_OID = 26
_INET = 869


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    dsn = _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP:
            c.execute(q)
    yield dsn
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP[:3]:
            c.execute(q)


def _described(dsn: str, query: str) -> tuple[list[int], list[tuple[object, ...]]]:
    """The columns' type OIDs and the rows."""
    with psycopg.connect(dsn, autocommit=True) as c:
        cur = c.execute(query)
        assert cur.description is not None
        return [d.type_code for d in cur.description], [tuple(r) for r in cur.fetchall()]


DESCRIBED = {
    "select max(v), min(v) from t681": ([_TEXT, _TEXT], [("cd", "ab")]),
    "select max(w), min(w) from t681": ([_TEXT, _TEXT], [("y", "x")]),
    "select max(n), min(n) from t681": ([_TEXT, _TEXT], [("nn", "nm")]),
    "select max(q), min(q) from t681": ([_TEXT, _TEXT], [("b", "a")]),
    "select max(e), min(e) from t681": ([_TEXT, _TEXT], [("ef", "ee")]),
    "select k, max(v) from t681 group by k": ([23, _TEXT], [(1, "cd")]),
    "select max(v) from t681 having max(v) > 'a'": ([_TEXT], [("cd",)]),
    "select max(v) filter (where k = 1) from t681": ([_TEXT], [("cd",)]),
    "select max(distinct v) from t681": ([_TEXT], [("cd",)]),
    "select max(v order by v) from t681": ([_TEXT], [("cd",)]),
    "select max(v) over (), min(n) over (order by n) from t681 order by 2": (
        [_TEXT, _TEXT],
        [("cd", "nm"), ("cd", "nm")],
    ),
    "select max('ab'::varchar)": ([_TEXT], [("ab",)]),
    "select max(x) from (values ('ab'::varchar(4)), ('cd')) s(x)": ([_TEXT], [("cd",)]),
    "select s.m, pg_typeof(s.m)::text from (select max(v) m from t681) s": (
        [_TEXT, _TEXT],
        [("cd", "text")],
    ),
    "with c as (select max(v) m from t681) select m from c": ([_TEXT], [("cd",)]),
    "select pg_typeof(max(v))::text, pg_typeof(min(n))::text, pg_typeof(max(q))::text,"
    " pg_typeof(max(e))::text from t681": ([_TEXT] * 4, [("text", "text", "text", "text")]),
}


@pytest.mark.backlog_fixed("RP-681")
@pytest.mark.parametrize("query", list(DESCRIBED))
def test_min_max_take_their_overloads_type(query: str, dsn: str) -> None:
    """`min` / `max` over a `varchar`, `name`, `"char"` or a domain over
    `varchar` resolve to `max(text)` and were `text` on PostgreSQL 15.19:
    in an aggregate, a window call, a subquery's column and `pg_typeof`.
    Here they were the input type."""
    assert _described(dsn, query) == DESCRIBED[query]


@pytest.mark.backlog_fixed("RP-681")
def test_a_views_min_max_column_is_text(dsn: str) -> None:
    """A view's `max(varchar)` column was `text` in `pg_attribute` on
    PostgreSQL 15.19 and `character varying` here."""
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute("create or replace view v681 as select max(v) m, min(e) e from t681")
        got = c.execute(
            "select a.attname::text, format_type(a.atttypid, a.atttypmod) from pg_attribute a"
            " join pg_class r on r.oid = a.attrelid where r.relname = 'v681' and a.attnum > 0"
            " order by a.attnum"
        ).fetchall()
        c.execute("drop view v681")
    assert got == [("m", "text"), ("e", "text")]


@pytest.mark.backlog_fixed("RP-706")
def test_min_max_coerce_their_input_to_the_overloads_type(dsn: str) -> None:
    """`max(regclass)` is `max(oid)` on PostgreSQL 15.19: each input is
    coerced to `oid` and the largest OID is the answer, described `oid`
    (`pg_class` 1259 over `pg_type` 1247); `max(cidr)` is `max(inet)`,
    described `inet`. Here both kept their input type (RP-706)."""
    assert _described(
        dsn,
        "select max(x)::oid::int8, min(x)::oid::int8, pg_typeof(max(x))::text"
        " from (values ('pg_class'::regclass), ('pg_type'::regclass)) s(x)",
    ) == ([20, 20, _TEXT], [(1259, 1247, "oid")])
    assert _described(dsn, "select max(x) from (values ('pg_type'::regtype)) s(x)") == (
        [_OID],
        [(71,)],
    )
    assert _described(
        dsn,
        "select max(x)::text, pg_typeof(max(x))::text from (values ('10.0.0.0/8'::cidr)) s(x)",
    ) == ([_TEXT, _TEXT], [("10.0.0.0/8", "inet")])
    with psycopg.connect(dsn, autocommit=True) as c:
        cur = c.execute("select max(x) from (values ('10.0.0.0/8'::cidr)) s(x)")
        assert cur.description is not None
        assert cur.description[0].type_code == _INET


@pytest.mark.backlog_fixed("RP-707")
@pytest.mark.parametrize(
    "query",
    [
        "select pg_typeof((select max(v) from t681))::text",
        "select pg_typeof((select upper(v) from t681 limit 1))::text",
        "select pg_typeof((select string_agg(v, ',') from t681))::text",
        "select pg_typeof((select 'a'::text))::text",
        "select pg_typeof((select 'a'))::text",
    ],
)
def test_pg_typeof_of_a_text_scalar_subquery(query: str, dsn: str) -> None:
    """`pg_typeof` of a scalar subquery whose column is `text` is `text` on
    PostgreSQL 15.19; here it was 42P18 `could not determine data type of
    parameter $1`, while one over an `int` or a bare `varchar` column
    answered: the resolved value's parameter was declared for every column
    type but `text`."""
    try:
        got: object = _described(dsn, query)
    except psycopg.Error as e:
        got = ("error", e.sqlstate, str(e).splitlines()[0])
    assert got == ([_TEXT], [("text",)])


_TEXT_SUBQUERY = "(select max(x) from (values ('uu'::text), ('tt')) s(x))"


@pytest.mark.backlog_fixed("RP-707")
@pytest.mark.parametrize(
    ("query", "expected"),
    [
        (f"select concat({_TEXT_SUBQUERY}, 1)", ([_TEXT], [("uu1",)])),
        (f"select format('%s!', {_TEXT_SUBQUERY})", ([_TEXT], [("uu!",)])),
        (f"select {_TEXT_SUBQUERY} || array['q']", ([1009], [(["uu", "q"],)])),
        (
            "select (select '12') + 1",
            ("error", "42883", "operator does not exist: text + integer"),
        ),
    ],
)
def test_a_text_scalar_subquery_is_typed_text(query: str, expected: object, dsn: str) -> None:
    """The same undeclared parameter reached every caller of a `text`
    scalar subquery: `concat` and `format` refused it with 42P18, `||`
    read it as an array literal (22P02 `malformed array literal`), and
    `(select '12') + 1` answered 13. Measured on PostgreSQL 15.19, each is
    the answer of a `text` value."""
    try:
        got: object = _described(dsn, query)
    except psycopg.Error as e:
        got = ("error", e.sqlstate, str(e).splitlines()[0])
    assert got == expected
