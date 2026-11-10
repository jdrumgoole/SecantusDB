"""What working RP-766 (an enum column of a VALUES list sorts by its text)
found.

An enum column read through a subquery or a CTE, out of a table or a VALUES
list, sorts and compares by its labels' text, where PostgreSQL keeps the
column's enum type and so its declaration order. Measured on a private
PostgreSQL 15.19 cluster, since every statement needs a `CREATE TYPE`.
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


@pytest.fixture(scope="module")
def _enum(rust_pg: object) -> object:
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        c.execute("create type e766 as enum ('z', 'a', 'm')")
        c.execute("create table t766 (m e766)")
        c.execute("insert into t766 values ('m'), ('z'), ('a')")
    return rust_pg


@pytest.mark.backlog("RP-768")
@pytest.mark.parametrize("query", list(DERIVED_ENUM))
def test_a_derived_enum_column_sorts_by_declaration(query: str, _enum: object) -> None:
    """An enum column read through a subquery or a CTE keeps its type on
    PostgreSQL 15.19, so it sorts by declaration, `z` before `a`. Here it
    sorts and compares by the labels' text."""
    with psycopg.connect(_dsn(_enum) + " dbname=postgres", autocommit=True) as c:
        assert [tuple(r) for r in c.execute(query).fetchall()] == DERIVED_ENUM[query]
