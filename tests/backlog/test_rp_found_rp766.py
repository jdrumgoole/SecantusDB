"""What working RP-766 (an enum column of a VALUES list sorts by its text)
found, fixed by RP-768.

An enum column read through a subquery or a CTE, out of a table or a VALUES
list, sorted and compared by its labels' text, where PostgreSQL keeps the
column's enum type and so its declaration order. So did every other type the
order rewrite acts on: a network type, citext, a range, and a column's
declared collation. Measured on a private PostgreSQL 15.19 cluster, since
every statement needs a `CREATE TYPE` or a `CREATE TABLE`.
"""

from __future__ import annotations

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

DERIVED_ENUM = {
    "select m::text as v from (select m from t766) q order by m": [("z",), ("a",), ("m",)],
    "select m::text as v from (select * from t766) q order by m": [("z",), ("a",), ("m",)],
    "select m::text as v from (select m from t766) q where m > 'z' order by 1": [
        ("a",),
        ("m",),
    ],
    "with w as (select m from t766) select m::text as v from w order by m": [
        ("z",),
        ("a",),
        ("m",),
    ],
    "select x::text as v from (select * from (values ('m'::e766), ('z'), ('a')) s(x)) q"
    " order by x": [("z",), ("a",), ("m",)],
}

# Each measured on PostgreSQL 15.19 over `_TYPES_SETUP`.
DERIVED_TYPES = {
    "select n::text as v from (select n from t768) q order by n": [
        ("9.0.0.1/32",),
        ("9.0.0.2/32",),
        ("10.0.0.1/32",),
    ],
    "select c::text as v from (select c from t768) q order by c": [("a",), ("B",), ("C",)],
    "select c::text as v from (select c from t768) q where c = 'b'": [("B",)],
    "select r::text as v from (select r from t768) q order by r": [
        ("[5,6)",),
        ("[7,8)",),
        ("[10,11)",),
    ],
    "select k as v from (select k from t768) q order by k": [("a",), ("B",), ("C",)],
    "select k as v from (select * from t768) q order by k": [("a",), ("B",), ("C",)],
    "select m::text as v from (select m from t768 union all select m from u768) q order by m": [
        ("z",),
        ("z",),
        ("a",),
        ("a",),
        ("m",),
        ("m",),
    ],
    "select m::text as v from (select 'm' union select m from u768) q(m) order by m": [
        ("z",),
        ("a",),
        ("m",),
    ],
    "select m::text as v from (select t768.* from t768) q order by m": [
        ("z",),
        ("a",),
        ("m",),
    ],
    "select m::text as v from (select u768.m from t768 join u768 using (i)) q order by m": [
        ("z",),
        ("a",),
        ("m",),
    ],
    "with w as (select m from t768), w2 as (select m from w)"
    " select m::text as v from w2 order by m": [("z",), ("a",), ("m",)],
    "select m::text as v from (with w as (select m from t768) select m from w) q order by m": [
        ("z",),
        ("a",),
        ("m",),
    ],
    "select m::text as v from (select m from (select m from t768) a) b order by m": [
        ("z",),
        ("a",),
        ("m",),
    ],
    "select max(m)::text as v from (select m from t768) q": [("m",)],
    "select mm::text as v from (select m as mm from t768) q order by mm": [
        ("z",),
        ("a",),
        ("m",),
    ],
    "select m::text as v from (select m from t768) q where m between 'z' and 'a' order by m": [
        ("z",),
        ("a",),
    ],
    "select s::text as v from (select m::text as s from t768) q order by s": [
        ("a",),
        ("m",),
        ("z",),
    ],
    "select string_agg(m::text, ',' order by m) as v from (select m from t768) q": [("z,a,m",)],
}

_TYPES_SETUP = (
    "create extension if not exists citext",
    "create type e768 as enum ('z', 'a', 'm')",
    'create table t768 (m e768, n inet, c citext, r int4range, k text collate "en-x-icu", i int)',
    "insert into t768 values ('m', '10.0.0.1', 'B', '[10,11)', 'B', 1),"
    " ('z', '9.0.0.1', 'a', '[5,6)', 'a', 2), ('a', '9.0.0.2', 'C', '[7,8)', 'C', 3)",
    "create table u768 (i int, m e768)",
    "insert into u768 values (1, 'a'), (2, 'z'), (3, 'm')",
)


@pytest.fixture(scope="module")
def _enum(rust_pg: object) -> object:
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        c.execute("create type e766 as enum ('z', 'a', 'm')")
        c.execute("create table t766 (m e766)")
        c.execute("insert into t766 values ('m'), ('z'), ('a')")
        for statement in _TYPES_SETUP:
            c.execute(statement)
    return rust_pg


@pytest.mark.backlog_fixed("RP-768")
@pytest.mark.parametrize("query", list(DERIVED_ENUM))
def test_a_derived_enum_column_sorts_by_declaration(query: str, _enum: object) -> None:
    """An enum column read through a subquery or a CTE keeps its type on
    PostgreSQL 15.19, so it sorts by declaration, `z` before `a`. It sorted
    and compared by the labels' text."""
    with psycopg.connect(_dsn(_enum) + " dbname=postgres", autocommit=True) as c:
        assert [tuple(r) for r in c.execute(query).fetchall()] == DERIVED_ENUM[query]


@pytest.mark.backlog_fixed("RP-768")
@pytest.mark.parametrize("query", list(DERIVED_TYPES))
def test_a_derived_column_keeps_its_type_and_collation(query: str, _enum: object) -> None:
    """A FROM subquery's or a WITH item's column that passes a column through
    keeps its type and collation on PostgreSQL 15.19 -- through a `*`, a
    qualified `*`, a set operation, a join, a nested subquery, and a WITH
    inside one -- so an inet, citext, range, enum or ICU-collated column
    sorts and compares as itself. It sorted as text."""
    with psycopg.connect(_dsn(_enum) + " dbname=postgres", autocommit=True) as c:
        assert [tuple(r) for r in c.execute(query).fetchall()] == DERIVED_TYPES[query]
