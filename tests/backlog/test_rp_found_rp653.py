"""Gap test of the divergence found while working RP-653 (a domain array
column in `pg_attribute`): a user column's `attndims` was always 0.

The expected answer was measured on a private, freshly initdb'd PostgreSQL
15.19 cluster; the test makes the objects it names and drops them after.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_RESET = [
    "drop table if exists t658l, t658c, t658m, t658p, t658a, t658mp, t658o",
    "drop table if exists t653n",
    "drop type if exists c658",
    "drop domain if exists d653n",
]


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        for q in _RESET:
            c.execute(q)


@pytest.mark.backlog_fixed("RP-658")
def test_array_column_attndims(dsn: str) -> None:
    """`attndims` was the number of dimensions the column was DECLARED with
    (`ColumnDef`'s `arrayBounds`), a domain's array included; a size in the
    brackets counts the same. It was 0 for every user column."""
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in [*_RESET, "create domain d653n as int"]:
            c.execute(q)
        c.execute("create table t653n (a int[], b int[][], c int, d int[3], e d653n[])")
        got = c.execute(
            "select attname, attndims from pg_attribute "
            "where attrelid = 't653n'::regclass and attnum > 0 order by attnum"
        ).fetchall()
    assert got == [("a", 1), ("b", 2), ("c", 0), ("d", 1), ("e", 1)]


def _ndims(c: psycopg.Connection, rel: str) -> list[tuple[str, int]]:
    return c.execute(
        "select attname, attndims from pg_attribute "
        "where attrelid = %s::regclass and attnum > 0 order by attnum",
        (rel,),
    ).fetchall()


@pytest.mark.backlog_fixed("RP-658")
def test_attndims_follows_the_written_definition(dsn: str) -> None:
    """Only a column whose definition WRITES the brackets counts them: the
    internal array name `_int4` is 0, and a column PostgreSQL defines from a
    type OID (`makeTypeNameFromOid`: LIKE, a partition's, an inherited one
    even when the child declares it too, `OF type`, CREATE TABLE AS) is 0
    whatever its type. ADD COLUMN and ALTER COLUMN TYPE count the new
    declaration, an ALTER on a parent reaches its child, and an index key
    column copies its table column's. Every literal was measured on a
    private PostgreSQL 15.19 cluster."""
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in [
            *_RESET,
            "create domain d653n as int",
            "create table t653n (a int[], b int[][], f _int4, g int array,"
            " h int array[4], i text[][][], e d653n[])",
        ]:
            c.execute(q)
        assert _ndims(c, "t653n") == [
            ("a", 1),
            ("b", 2),
            ("f", 0),
            ("g", 1),
            ("h", 1),
            ("i", 3),
            ("e", 1),
        ]
        c.execute("create table t658l (like t653n including all)")
        assert {n for _, n in _ndims(c, "t658l")} == {0}
        c.execute("create table t658c (x int[][]) inherits (t653n)")
        assert _ndims(c, "t658c")[0] == ("a", 0)
        assert _ndims(c, "t658c")[-1] == ("x", 2)
        c.execute("create table t658m (a int[][][]) inherits (t653n)")
        assert _ndims(c, "t658m")[0] == ("a", 0)
        c.execute("create table t658a as select a, b from t653n")
        assert _ndims(c, "t658a") == [("a", 0), ("b", 0)]
        c.execute("create table t658p (a int[][], k int) partition by range (k)")
        assert _ndims(c, "t658p") == [("a", 2), ("k", 0)]
        c.execute("create table t658mp partition of t658p for values from (0) to (10)")
        assert _ndims(c, "t658mp") == [("a", 0), ("k", 0)]
        c.execute("create type c658 as (p int[][], q int)")
        c.execute("create table t658o of c658")
        assert _ndims(c, "t658o") == [("p", 0), ("q", 0)]
        c.execute("create index t658_a_idx on t653n (a)")
        assert c.execute(
            "select attndims from pg_attribute where attrelid = 't658_a_idx'::regclass"
        ).fetchall() == [(1,)]
        c.execute("alter table t653n add column j int[][]")
        c.execute("alter table t653n alter column a type int[][][]")
        c.execute("alter table t653n alter column b type int[]")
        assert _ndims(c, "t653n")[:2] == [("a", 3), ("b", 1)]
        assert _ndims(c, "t653n")[-1] == ("j", 2)
        assert _ndims(c, "t658c")[0] == ("a", 3)
        assert c.execute(
            "select attndims from pg_attribute where attrelid = 't658_a_idx'::regclass"
        ).fetchall() == [(3,)]
