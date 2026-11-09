"""Gap tests for how long the Rust PG server takes over catalog queries real
clients send (`tasks/backlog-rust-postgres.md`, "Found by the corpus test").

Found on 2026-10-21, when `tests/test_rust_pg_corpora.py` first ran every
corpus: `psql_describe` took 268 s, 263 of them in four runs of the column
query psql sends for `\\d <table>`.
"""

from __future__ import annotations

import time
from typing import Any

import pytest

#: psql 15's column query for `\d`, cut to the correlated subquery that holds
#: all the time: a comma join of pg_collation and pg_type, filtered by
#: equalities on the OUTER row's columns.
COLLATION_QUERY = (
    "SELECT a.attname, (SELECT c.collname FROM pg_catalog.pg_collation c, "
    "pg_catalog.pg_type t WHERE c.oid = a.attcollation AND t.oid = a.atttypid "
    "AND a.attcollation <> t.typcollation) FROM pg_catalog.pg_attribute a "
    "WHERE a.attrelid = 'rp_cx'::pg_catalog.regclass AND a.attnum > 0 "
    "AND NOT a.attisdropped ORDER BY a.attnum"
)

#: PostgreSQL 15.19 answered in 3.5 ms. A debug `secantusd-pg` took 64 s,
#: for one column as for three. The query is timed WARM, on its second run:
#: the first query after any DDL re-decodes the whole catalog (RP-715), which
#: costs a debug build 0.6 s on a fast laptop and took a slow CI runner past
#: this budget on 2026-11-09 (PR #2552). Warm, a debug build answers in about
#: 0.016 s, so the budget is far from both answers again.
BUDGET_SECONDS = 2.0


@pytest.mark.backlog_fixed("RP-077")
def test_psql_describe_collation_subquery(pg: Any) -> None:
    """PostgreSQL 15.19 answers `[('v', 'C')]` (measured on a private
    cluster: the scenario creates a table) in milliseconds, because the
    subquery's equalities on the outer row pick one pg_collation row and one
    pg_type row. Evaluating the 911 x 181 cross product is the same answer,
    a minute later.

    The first run after the `CREATE TABLE` is not timed: it pays for
    re-reading the catalog after DDL (RP-715). Only decoded catalog rows and
    plans are cached, never a result, so the second run still evaluates the
    subquery for every column."""
    pg.execute('CREATE TABLE rp_cx (v text COLLATE "C")')
    assert pg.execute(COLLATION_QUERY).fetchall() == [("v", "C")]
    start = time.monotonic()
    rows = pg.execute(COLLATION_QUERY).fetchall()
    elapsed = time.monotonic() - start
    assert rows == [("v", "C")]
    assert elapsed < BUDGET_SECONDS, f"{elapsed:.1f} s for one column"


@pytest.mark.backlog("RP-715")
def test_the_first_catalog_query_after_ddl_costs_what_the_next_does(pg: Any) -> None:
    """PostgreSQL 15.19 (private cluster, 2026-11-09) answers the collation
    query in 0.9 ms straight after a `CREATE TABLE` and 0.2 ms after that:
    DDL invalidates only the cache entries it changed (`inval.c`). A debug
    `secantusd-pg` took 0.58 s and then 0.016 s, because any DDL makes the
    next catalog read decode every catalog collection again.

    A ratio, not a budget, so the verdict holds on any runner and any build:
    the fastest first-after-DDL run of three against the fastest warm run."""
    firsts, warms = [], []
    for i in range(3):
        pg.execute(f'CREATE TABLE rp715_{i} (v text COLLATE "C")')
        query = COLLATION_QUERY.replace("'rp_cx'", f"'rp715_{i}'")
        start = time.monotonic()
        assert pg.execute(query).fetchall() == [("v", "C")]
        firsts.append(time.monotonic() - start)
        for _ in range(2):
            start = time.monotonic()
            pg.execute(query).fetchall()
            warms.append(time.monotonic() - start)
    first, warm = min(firsts), min(warms)
    assert first < 3 * warm + 0.02, (
        f"first query after DDL {first * 1000:.0f} ms, warm {warm * 1000:.0f} ms"
    )
