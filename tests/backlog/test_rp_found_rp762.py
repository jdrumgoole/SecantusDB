"""What working RP-762 (an ORDER BY comparison over a network value compares
the text) found.

An enum column of a VALUES list is ordered by its labels' text, where a
table's enum column is ordered by declaration. Measured on a private
PostgreSQL 15.19 cluster, since every statement needs a `CREATE TYPE`.
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
}


@pytest.fixture(scope="module")
def _enum(rust_pg: object) -> object:
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        c.execute("create type e766 as enum ('z', 'a', 'm')")
    return rust_pg


@pytest.mark.backlog("RP-766")
@pytest.mark.parametrize("query", list(VALUES_ENUM))
def test_a_values_enum_column_sorts_by_declaration(query: str, _enum: object) -> None:
    """An enum column of a VALUES list sorts by declaration order on
    PostgreSQL 15.19, `z` before `a`, as a table's enum column does here.
    Here it sorts by the labels' text, `a, m, z`."""
    with psycopg.connect(_dsn(_enum) + " dbname=postgres", autocommit=True) as c:
        assert [tuple(r) for r in c.execute(query).fetchall()] == VALUES_ENUM[query]
