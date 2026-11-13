"""Gap tests of the divergences found while working RP-921 (a `CREATE
TABLE`'s index builds read only the new table's indexes).

The expected answers were measured on a private, freshly initdb'd
PostgreSQL 15.19 cluster (2026-11-13). The build before RP-921 answered as
the one after it, so none of these is that change's.
"""

from __future__ import annotations

from typing import Any

import pytest

SIZES = (
    "SELECT c.relname, c.relpages, c.reltuples FROM pg_class c "
    "JOIN pg_index i ON i.indexrelid = c.oid "
    "JOIN pg_class t ON t.oid = i.indrelid "
    "WHERE t.relname IN ({}) ORDER BY 1"
)


def _partitioned(pg: Any, p: str) -> None:
    """`p`, range-partitioned with a primary key, a unique index and a plain
    one, each named by the user; `p_1` a partitioned partition of it, and
    `p_1a` a leaf partition of that."""
    pg.execute(f"DROP TABLE IF EXISTS {p} CASCADE")
    pg.execute(f"CREATE TABLE {p} (a int PRIMARY KEY, b int, c int) PARTITION BY RANGE (a)")
    pg.execute(f"CREATE UNIQUE INDEX {p}_ab ON {p} (a, b)")
    pg.execute(f"CREATE INDEX {p}_c ON {p} (c)")
    pg.execute(
        f"CREATE TABLE {p}_1 PARTITION OF {p} FOR VALUES FROM (0) TO (100) PARTITION BY RANGE (a)"
    )
    pg.execute(f"CREATE TABLE {p}_1a PARTITION OF {p}_1 FOR VALUES FROM (0) TO (50)")


@pytest.mark.backlog("RP-930")
def test_a_partitions_copy_of_a_named_index_is_named_by_its_columns(pg: Any) -> None:
    """PostgreSQL names a partition's copy of a partitioned table's index
    afresh (`ChooseRelationName` over the key columns, `DefineIndex`), never
    from the parent index's name: `p_1_a_b_idx` for the user's `p_ab`, and
    `p_1_c_idx` for `p_c`. `secantusd-pg` took the parent's name minus the
    parent table's prefix, answering `p_1_ab` and `p_1_c` (2026-11-13)."""
    _partitioned(pg, "rp921n")
    names = [
        r[0]
        for r in pg.execute(
            SIZES.format("'rp921n_1', 'rp921n_1a'").replace(
                "c.relname, c.relpages, c.reltuples", "c.relname"
            )
        ).fetchall()
    ]
    assert names == [
        "rp921n_1_a_b_idx",
        "rp921n_1_c_idx",
        "rp921n_1_pkey",
        "rp921n_1a_a_b_idx",
        "rp921n_1a_c_idx",
        "rp921n_1a_pkey",
    ]


@pytest.mark.backlog("RP-931")
def test_a_partitioned_tables_indexes_have_no_pages(pg: Any) -> None:
    """A partitioned table's index has no storage of its own (`RELKIND_
    PARTITIONED_INDEX`), so PostgreSQL shows it at 0 pages and 0 tuples:
    the parent's and the partitioned partition's indexes alike.
    `secantusd-pg` answered 1 page for each primary key, and for the
    parent's own indexes (2026-11-13)."""
    _partitioned(pg, "rp921p")
    rows = pg.execute(SIZES.format("'rp921p', 'rp921p_1'")).fetchall()
    assert [(r[1], r[2]) for r in rows] == [(0, 0.0)] * 6, rows


@pytest.mark.backlog("RP-932")
def test_a_leaf_partitions_inherited_indexes_are_built(pg: Any) -> None:
    """A leaf partition's copies of its ancestors' indexes are built over
    its heap when it is created, as each of its own constraints' are, so
    `index_update_stats` records 1 page (the metapage) and 0 tuples for
    every one. `secantusd-pg` showed the copies of the user's indexes as
    never built: 0 pages, -1 tuples (2026-11-13)."""
    _partitioned(pg, "rp921l")
    rows = pg.execute(SIZES.format("'rp921l_1a'")).fetchall()
    assert [(r[1], r[2]) for r in rows] == [(1, 0.0)] * 3, rows


@pytest.mark.backlog("RP-929")
def test_a_hash_index_on_an_empty_table_is_recorded_as_built(pg: Any) -> None:
    """`CREATE INDEX ... USING hash` on an empty table: PostgreSQL's
    `hashbuild` lays out the metapage, the bitmap page and the initial
    buckets, and `index_update_stats` records 6 pages and 0 tuples.
    `secantusd-pg` showed it as never built: 0 pages, -1 tuples
    (2026-11-13)."""
    pg.execute("DROP TABLE IF EXISTS rp921h")
    pg.execute("CREATE TABLE rp921h (a int, b text)")
    pg.execute("CREATE INDEX rp921h_h ON rp921h USING hash (a)")
    rows = pg.execute(SIZES.format("'rp921h'")).fetchall()
    assert rows == [("rp921h_h", 6, 0.0)]
