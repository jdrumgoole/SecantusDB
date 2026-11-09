"""Gap test of the divergence found while working RP-658 (`attndims`):
ALTER COLUMN TYPE between two array types refuses the element's assignment
cast.

The expected answers were measured on a private, freshly initdb'd
PostgreSQL 15.19 cluster; the test makes the table it names and drops it
after.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.conftest import pg_answer
from tests.backlog.test_rp_batch26 import _dsn


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        c.execute("drop table if exists t658z")


@pytest.mark.backlog_fixed("RP-662")
def test_alter_type_between_array_types(dsn: str) -> None:
    """An array's assignment cast is its element's (`find_coercion_pathway`
    over `typelem`, an `array_coerce` path), so on PostgreSQL 15.19 `int[]`
    to `int8[]`, `smallint[]` to `int[]` and `int8[]` to `numeric[]` each
    ALTER without USING and keep the values; only a pair with no element
    cast (`text` to `int[]`) is 42804. Each of the first three was refused
    42804 until RP-662 ported `find_coercion_pathway`."""
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute("drop table if exists t658z")
        c.execute("create table t658z (a int[], s smallint[], t text)")
        c.execute("insert into t658z values ('{1,2}', '{3}', '{4}')")
        got = [
            pg_answer(c, "alter table t658z alter column a type int8[]")[:2],
            pg_answer(c, "alter table t658z alter column s type int[]")[:2],
            pg_answer(c, "alter table t658z alter column a type numeric[]")[:2],
            pg_answer(c, "alter table t658z alter column t type int[]")[:2],
            pg_answer(
                c,
                "select a::text, pg_typeof(a)::text, s::text, pg_typeof(s)::text from t658z",
            ),
        ]
    assert got == [
        ("ok", "ALTER TABLE"),
        ("ok", "ALTER TABLE"),
        ("ok", "ALTER TABLE"),
        ("err", "42804"),
        ("ok", [("{1,2}", "numeric[]", "{3}", "integer[]")]),
    ]


@pytest.mark.backlog("RP-663")
def test_composite_attribute_attndims(dsn: str) -> None:
    """A composite type's attribute records the dimensions it was declared
    with too (`DefineCompositeType` builds its descriptor with
    `BuildDescForRelation`): after `create type c658z as (p int[][], q int,
    r text[])` PostgreSQL 15.19's `pg_attribute` has 2, 0 and 1."""
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute("drop type if exists c658z")
        c.execute("create type c658z as (p int[][], q int, r text[])")
        got = pg_answer(
            c,
            "select attname::text, attndims from pg_attribute "
            "where attrelid = 'c658z'::regclass order by attnum",
        )
        c.execute("drop type c658z")
    assert got == ("ok", [("p", 2), ("q", 0), ("r", 1)])
