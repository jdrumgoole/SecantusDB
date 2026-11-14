"""Gap tests of the divergences found while working RP-931 (a partitioned
table's indexes show no pages in `pg_class`).

The expected answers were measured on PostgreSQL 15.19 (2026-11-14).
"""

from __future__ import annotations

from typing import Any

import pytest


@pytest.mark.backlog("RP-949")
def test_a_partitioned_tables_indexes_have_no_size(pg: Any) -> None:
    """A partitioned index has no storage, so PostgreSQL sizes it at 0, and
    a partitioned table's indexes at 0 together, even with rows in its
    partitions. `secantusd-pg` answered a page per index, sized from the
    root's B-tree model (2026-11-14)."""
    pg.execute("DROP TABLE IF EXISTS rp931s CASCADE")
    pg.execute("CREATE TABLE rp931s (a int PRIMARY KEY, b int, c int) PARTITION BY RANGE (a)")
    pg.execute("CREATE INDEX rp931s_c ON rp931s (c)")
    pg.execute("CREATE TABLE rp931s_1 PARTITION OF rp931s FOR VALUES FROM (0) TO (100)")
    pg.execute("INSERT INTO rp931s SELECT g, g, g FROM generate_series(0, 99) g")
    row = pg.execute(
        "SELECT pg_relation_size('rp931s_pkey'), pg_relation_size('rp931s_c'), "
        "pg_indexes_size('rp931s'), pg_total_relation_size('rp931s')"
    ).fetchone()
    assert row == (0, 0, 0, 0)
