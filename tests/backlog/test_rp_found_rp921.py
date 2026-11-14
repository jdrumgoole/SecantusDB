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


@pytest.mark.backlog_fixed("RP-931")
def test_a_partitioned_tables_indexes_have_no_pages(pg: Any) -> None:
    """A partitioned table's index has no storage of its own (`RELKIND_
    PARTITIONED_INDEX`), so PostgreSQL showed it at 0 pages and 0 tuples:
    the parent's and the partitioned partition's indexes alike.
    `secantusd-pg` answered 1 page for each primary key, and for the
    parent's own indexes, and 0 pages, -1 tuples for the partitioned
    partition's copies of the others (2026-11-13)."""
    _partitioned(pg, "rp921p")
    rows = pg.execute(SIZES.format("'rp921p', 'rp921p_1'")).fetchall()
    assert [(r[1], r[2]) for r in rows] == [(0, 0.0)] * 6, rows


@pytest.mark.backlog_fixed("RP-931")
def test_maintenance_leaves_a_partitioned_tables_indexes_without_pages(pg: Any) -> None:
    """No VACUUM, ANALYZE, TRUNCATE or REINDEX writes a partitioned index's
    `pg_class` row (`vac_update_relstats` and `index_update_stats` reach
    only relations with storage): PostgreSQL 15.19 still showed 0 pages,
    0 tuples after each, with rows in the partitions (measured on a
    one-level tree, and after ANALYZE on this one, 2026-11-14)."""
    _partitioned(pg, "rp921m")
    pg.execute("INSERT INTO rp921m SELECT g, g, g FROM generate_series(0, 49) g")
    for statement in ("VACUUM ANALYZE", "TRUNCATE", "REINDEX TABLE"):
        pg.execute(f"{statement} rp921m")
        rows = pg.execute(SIZES.format("'rp921m', 'rp921m_1'")).fetchall()
        assert [(r[1], r[2]) for r in rows] == [(0, 0.0)] * 6, (statement, rows)


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


@pytest.mark.backlog_fixed("RP-929")
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


@pytest.mark.backlog_fixed("RP-929")
def test_every_hash_index_build_records_its_size(pg: Any) -> None:
    """Each build of a hash index records its pages (the metapage, the
    bitmap page and the buckets `_hash_init` sizes from the heap's
    estimate) and its index tuples, NULL keys and rows outside a partial
    index's WHERE left out; TRUNCATE forgets them, REINDEX records them
    again. PostgreSQL 15.19's answers on a private cluster (2026-11-14):
    `secantusd-pg` answered 0 pages, -1 tuples for every one, whether its
    key's hash was reproduced (int, text, bigint) or not (numeric, a
    collated text, a partial or an expression index)."""
    for t in ("rp929a", "rp929b", "rp929c", "rp929d"):
        pg.execute(f"DROP TABLE IF EXISTS {t}")
    pg.execute("CREATE TABLE rp929a (a int, b text)")
    pg.execute("INSERT INTO rp929a SELECT g, g::text FROM generate_series(1, 1000) g")
    pg.execute("INSERT INTO rp929a VALUES (NULL, NULL)")
    pg.execute("CREATE INDEX rp929a_h ON rp929a USING hash (a)")
    pg.execute("CREATE INDEX rp929a_t ON rp929a USING hash (b)")
    pg.execute("CREATE TABLE rp929b (a int)")
    pg.execute("INSERT INTO rp929b SELECT g FROM generate_series(1, 100000) g")
    pg.execute("CREATE INDEX rp929b_h ON rp929b USING hash (a)")
    pg.execute("CREATE TABLE rp929c (a bigint)")
    pg.execute("CREATE INDEX rp929c_h ON rp929c USING hash (a) WITH (fillfactor = 10)")
    pg.execute('CREATE TABLE rp929d (a numeric, b text COLLATE "C")')
    pg.execute("CREATE INDEX rp929d_h ON rp929d USING hash (a)")
    pg.execute("CREATE INDEX rp929d_t ON rp929d USING hash (b)")
    pg.execute("CREATE INDEX rp929d_p ON rp929d USING hash (a) WHERE a > 0")
    tables = "'rp929a', 'rp929b', 'rp929c', 'rp929d'"
    assert pg.execute(SIZES.format(tables)).fetchall() == [
        ("rp929a_h", 6, 1000.0),
        ("rp929a_t", 6, 1000.0),
        ("rp929b_h", 514, 100000.0),
        ("rp929c_h", 66, 0.0),
        ("rp929d_h", 4, 0.0),
        ("rp929d_p", 4, 0.0),
        ("rp929d_t", 4, 0.0),
    ]
    pg.execute("DROP TABLE IF EXISTS rp929e")
    pg.execute("CREATE TABLE rp929e (a numeric, b int)")
    pg.execute("INSERT INTO rp929e SELECT g, g FROM generate_series(1, 1000) g")
    pg.execute("INSERT INTO rp929e VALUES (NULL, 5)")
    pg.execute("CREATE INDEX rp929e_p ON rp929e USING hash (b) WHERE a > 400")
    pg.execute("CREATE INDEX rp929e_n ON rp929e USING hash (a)")
    pg.execute("CREATE INDEX rp929e_e ON rp929e USING hash ((b % 7)) WHERE b > 100")
    assert pg.execute(SIZES.format("'rp929e'")).fetchall() == [
        ("rp929e_e", 6, 900.0),
        ("rp929e_n", 6, 1000.0),
        ("rp929e_p", 6, 600.0),
    ]
    pg.execute("TRUNCATE rp929b")
    pg.execute("REINDEX INDEX rp929a_h")
    assert pg.execute(SIZES.format("'rp929a', 'rp929b'")).fetchall() == [
        ("rp929a_h", 6, 1000.0),
        ("rp929a_t", 6, 1000.0),
        ("rp929b_h", 0, -1.0),
    ]
