"""Gap tests of the divergences found while working RP-929 (a hash index's
build records its size).

The expected answers were measured on a private, freshly initdb'd
PostgreSQL 15.19 cluster (2026-11-14). Before RP-929 `secantusd-pg` showed
every hash index at 0 pages, -1 tuples, so none of these is that change's.
"""

from __future__ import annotations

from typing import Any

import pytest

SIZE = "SELECT relpages, reltuples FROM pg_class WHERE relname = '{}'"


@pytest.mark.backlog("RP-954")
def test_analyze_and_vacuum_record_a_hash_indexs_size(pg: Any) -> None:
    """ANALYZE and VACUUM record each index's pages and tuples
    (`vac_update_relstats`); a hash index's VACUUM always reports them, as
    `hashvacuumcleanup` returns its stats every time. A hash index built
    on an empty `(a int)` table has 10 pages; 3,000 rows split it into the
    splitpoint of 16 buckets, 18 pages, which ANALYZE records with 3,000
    tuples, and a VACUUM after deleting 1,000 rows with 2,000.
    `secantusd-pg` kept the build's 10 pages and 0 tuples after both
    (2026-11-14)."""
    pg.execute("DROP TABLE IF EXISTS rp929m")
    pg.execute("CREATE TABLE rp929m (a int)")
    pg.execute("CREATE INDEX rp929m_h ON rp929m USING hash (a)")
    pg.execute("INSERT INTO rp929m SELECT g FROM generate_series(1, 3000) g")
    pg.execute("ANALYZE rp929m")
    assert pg.execute(SIZE.format("rp929m_h")).fetchall() == [(18, 3000.0)]
    pg.execute("DELETE FROM rp929m WHERE a <= 1000")
    pg.execute("VACUUM rp929m")
    assert pg.execute(SIZE.format("rp929m_h")).fetchall() == [(18, 2000.0)]


@pytest.mark.backlog("RP-955")
def test_a_hash_index_has_its_pages(pg: Any) -> None:
    """`pg_relation_size` of a hash index is its blocks: 6 pages (49152)
    built on an empty `(a int, b text)` table, and 18 (147456) once 3,000
    rows have split a 10-page one; `pg_indexes_size` of the first table,
    with two of them, 98304. `secantusd-pg` answered 0 for each index and
    16384 for the table (2026-11-14): the size functions read the B-tree,
    GIN and GiST models, and estimate any other index."""
    pg.execute("DROP TABLE IF EXISTS rp929s")
    pg.execute("DROP TABLE IF EXISTS rp929g")
    pg.execute("CREATE TABLE rp929s (a int, b text)")
    pg.execute("CREATE INDEX rp929s_h ON rp929s USING hash (a)")
    pg.execute("CREATE INDEX rp929s_t ON rp929s USING hash (b)")
    pg.execute("CREATE TABLE rp929g (a int)")
    pg.execute("CREATE INDEX rp929g_h ON rp929g USING hash (a)")
    pg.execute("INSERT INTO rp929g SELECT g FROM generate_series(1, 3000) g")
    row = pg.execute(
        "SELECT pg_relation_size('rp929s_h'), pg_indexes_size('rp929s'), "
        "pg_relation_size('rp929g_h')"
    ).fetchone()
    assert tuple(row) == (49152, 98304, 147456)


@pytest.mark.backlog("RP-956")
def test_duplicate_hash_keys_fill_overflow_pages(pg: Any) -> None:
    """A bucket whose primary page is full chains overflow pages
    (`_hash_addovflpage`): 1,000 rows of one key, all in one bucket, leave
    a hash index of 12 pages where 4 buckets alone make 6. `secantusd-pg`
    answered 6: its metapage model counts the buckets, not the tuples in
    each, so it has no overflow pages (2026-11-14)."""
    pg.execute("DROP TABLE IF EXISTS rp929o")
    pg.execute("CREATE TABLE rp929o (a int)")
    pg.execute("INSERT INTO rp929o SELECT 1 FROM generate_series(1, 1000)")
    pg.execute("CREATE INDEX rp929o_h ON rp929o USING hash (a)")
    assert pg.execute(SIZE.format("rp929o_h")).fetchall() == [(12, 1000.0)]
