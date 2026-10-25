"""Batch 81 of the Rust PG server (2026-10-25): what an index scan marks
(RP-196).

An index scan marks LP_DEAD the B-tree entries whose heap versions it finds
dead to all (`kill_prior_tuple`, `_bt_killitems`), and an insert into a full
leaf deletes them before it splits. Since batch 81 this server marks them for
a read the planner prices as an index scan, or an ORDER BY it reads through
an index. A read that returns one row is not priced, so a primary-key lookup
marks nothing. Measured on a private PostgreSQL 15.19 cluster.
"""

from __future__ import annotations

from typing import Any

import pytest


@pytest.mark.backlog("RP-196")
def test_primary_key_lookups_mark_the_dead_versions(rust_pg: Any) -> None:
    """PostgreSQL 15.19: a 2,000-row table updated three times, with each
    key looked up by its primary key between the updates. The lookups'
    index scans mark every older version, so the next update's inserts
    delete them and the index stays at 13 pages; unmarked, it splits to 14."""
    with rust_pg.connect() as conn:
        conn.execute("drop table if exists rp81_k")
        conn.execute(
            "create table rp81_k (a int primary key, b int) with (autovacuum_enabled = off)"
        )
        conn.execute("insert into rp81_k select g, 0 from generate_series(1, 2000) g")
        sizes = []
        for _ in range(3):
            conn.execute("update rp81_k set b = b + 1")
            for k in range(1, 2001):
                conn.execute("select b from rp81_k where a = %s", (k,)).fetchall()
            sizes.append(
                conn.execute("select pg_relation_size('rp81_k_pkey') / 8192").fetchone()[0]
            )
        conn.execute("drop table rp81_k")
    assert sizes == [13, 13, 13]
