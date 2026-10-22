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
#: for one column as for three; the budget is far from both, so neither a
#: slow CI runner nor a fast laptop moves the verdict.
BUDGET_SECONDS = 2.0


@pytest.mark.backlog_fixed("RP-077")
def test_psql_describe_collation_subquery(pg: Any) -> None:
    """PostgreSQL 15.19 answers `[('v', 'C')]` (measured on a private
    cluster: the scenario creates a table) in milliseconds, because the
    subquery's equalities on the outer row pick one pg_collation row and one
    pg_type row. Evaluating the 911 x 181 cross product is the same answer,
    a minute later."""
    pg.execute('CREATE TABLE rp_cx (v text COLLATE "C")')
    start = time.monotonic()
    rows = pg.execute(COLLATION_QUERY).fetchall()
    elapsed = time.monotonic() - start
    assert rows == [("v", "C")]
    assert elapsed < BUDGET_SECONDS, f"{elapsed:.1f} s for one column"
