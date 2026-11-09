"""Gap tests of the divergences found while working RP-657 (a domain column
reached through a join, a WITH or a set operation keeps its domain in an
array):

- a set operation of a `char(n)` domain column and a `text` one drops the
  domain's blanks;
- `ARRAY(select * ...)` over a one-column FROM subquery of a domain column
  is typed by the domain's base;
- a LATERAL subquery's column that is an outer domain column is typed by
  the domain's base.

The expected answers were measured on a private, freshly initdb'd
PostgreSQL 15.19 cluster with `_SETUP`, which each statement runs after.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "drop table if exists t657d, t657e cascade",
    "drop domain if exists d657c cascade",
    "create domain d657c as char(3)",
    "create table t657d (a d657c, i int)",
    "insert into t657d values ('a b', 1), ('xy', 2)",
    "create table t657e (a d657c, i int)",
    "insert into t657e values ('e', 1)",
]


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        for q in _SETUP[:2]:
            c.execute(q)


def _connect(dsn: str) -> psycopg.Connection:
    c = psycopg.connect(dsn, autocommit=True)
    for q in _SETUP:
        c.execute(q)
    return c


def _answer(c: psycopg.Connection, query: str) -> tuple[object, ...]:
    """The result's column types by name, and its rows as text."""
    try:
        cur = c.execute(query)
    except psycopg.Error as e:
        return ("error", e.sqlstate, str(e).splitlines()[0])
    assert cur.description is not None
    types = [
        c.execute("select format_type(%s::oid, null)", (d.type_code,)).fetchone()
        for d in cur.description
    ]
    return ([t[0] for t in types if t], [tuple(str(v) for v in r) for r in cur.fetchall()])


@pytest.mark.backlog_fixed("RP-672")
def test_set_operation_of_char_domain_and_text_keeps_blanks(dsn: str) -> None:
    """`select array(select a from t657d union all select a::text from
    t657e)` is `character[]` on PostgreSQL 15.19, each value with its
    blanks: `{"a b","xy ",e}`. Here `xy` lost its blank: the set
    operation's column has no modifier, and the stored value was not padded
    (RP-672)."""
    with _connect(dsn) as c:
        got = _answer(c, "select array(select a from t657d union all select a::text from t657e)")
    assert got == (["character[]"], [("['a b', 'xy ', 'e']",)])


@pytest.mark.backlog("RP-673")
def test_array_of_star_over_a_domain_subquery_is_the_domains(dsn: str) -> None:
    """`select array(select * from (select a from t657d) s order by 1)` is
    the domain's array `d657c[]` on PostgreSQL 15.19, as `select a` there
    is. Here it is `character[]`."""
    with _connect(dsn) as c:
        got = _answer(c, "select array(select * from (select a from t657d) s order by 1)")
    assert got == (["d657c[]"], [('{"a b","xy "}',)])


@pytest.mark.backlog("RP-674")
def test_lateral_column_of_an_outer_domain_column_is_the_domains(dsn: str) -> None:
    """A LATERAL subquery's column that is the outer row's domain column is
    of the domain on PostgreSQL 15.19 (its Var's type): `pg_typeof(b)` is
    `d657c` and `array[b]` the domain's array, with the padding. Here they
    are `character` and `character[]`, without it."""
    with _connect(dsn) as c:
        got = _answer(
            c,
            "select pg_typeof(b), array[b] from t657d x"
            " cross join lateral (select x.a b) l order by i",
        )
    assert got == (
        ["regtype", "d657c[]"],
        [("d657c", '{"a b"}'), ("d657c", '{"xy "}')],
    )
