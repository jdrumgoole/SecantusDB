"""What working RP-762 (an ORDER BY comparison over a network value compares
the text) found.

An enum column of a VALUES list was ordered by its labels' text, where a
table's enum column is ordered by declaration (RP-766, fixed). Measured on a
private PostgreSQL 15.19 cluster, since every statement needs a `CREATE TYPE`.
"""

from __future__ import annotations

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

VALUES_ENUM = {
    "select x::text as v from (values ('a'::e766), ('z'), ('m')) s(x) order by x": [
        ("z",),
        ("a",),
        ("m",),
    ],
    "select x::text as v from (values ('a'::e766), ('z'), ('m'), (null)) s(x)"
    " order by x > 'a'::e766, x": [("z",), ("a",), ("m",), (None,)],
    # The cast in a later row types the column too, as do a CTE's VALUES and
    # a set operation's first leg (measured with RP-766's fix).
    "select x::text as v from (values ('m'), ('z'::e766), ('a')) s(x) order by x": [
        ("z",),
        ("a",),
        ("m",),
    ],
    "with w(x) as (values ('a'::e766), ('z'), ('m')) select x::text as v from w order by x": [
        ("z",),
        ("a",),
        ("m",),
    ],
    "select x::text as v from (select 'a'::e766 as x union all select 'z'"
    " union all select 'm') s order by x": [("z",), ("a",), ("m",)],
    # Comparisons, min / max, a window's and an aggregate's ORDER BY.
    "select x::text as v from (values ('m'::e766, 1), ('z', 2), ('a', 3)) s(x, y)"
    " where x > 'z' order by x": [("a",), ("m",)],
    "select x::text, x > 'a' from (values ('a'::e766), ('z'), ('m')) s(x) order by 1": [
        ("a", False),
        ("m", True),
        ("z", False),
    ],
    "select min(x)::text, max(x)::text from (values ('a'::e766), ('z'), ('m')) s(x)": [("z", "m")],
    "select x::text, rank() over (order by x) from (values ('a'::e766), ('z'), ('m')) s(x)"
    " order by 1": [("a", 2), ("m", 3), ("z", 1)],
    "select string_agg(x::text, ',' order by x) from (values ('m'::e766), ('z'), ('a')) s(x)": [
        ("z,a,m",)
    ],
}


@pytest.fixture(scope="module")
def _enum(rust_pg: object) -> object:
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        c.execute("create type e766 as enum ('z', 'a', 'm')")
    return rust_pg


@pytest.mark.backlog_fixed("RP-766")
@pytest.mark.parametrize("query", list(VALUES_ENUM))
def test_a_values_enum_column_sorts_by_declaration(query: str, _enum: object) -> None:
    """An enum column of a VALUES list sorts by declaration order on
    PostgreSQL 15.19, `z` before `a`, as a table's enum column does. It
    sorted by the labels' text, `a, m, z`, until a row's cast to the enum
    typed the derived column (RP-766)."""
    with psycopg.connect(_dsn(_enum) + " dbname=postgres", autocommit=True) as c:
        assert [tuple(r) for r in c.execute(query).fetchall()] == VALUES_ENUM[query]
