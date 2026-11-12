"""Gap tests for how long the Rust PG server takes over catalog queries real
clients send (`tasks/backlog-rust-postgres.md`, "Found by the corpus test").

Found on 2026-10-21, when `tests/test_rust_pg_corpora.py` first ran every
corpus: `psql_describe` took 268 s, 263 of them in four runs of the column
query psql sends for `\\d <table>`.
"""

from __future__ import annotations

import time
from collections.abc import Iterator
from pathlib import Path
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
#: the first query after any DDL rebuilt the whole of `pg_attribute` (RP-715),
#: which cost a debug build 0.6 s on a fast laptop and took a slow CI runner
#: past this budget on 2026-11-09 (PR #2552); it still grows with the
#: schema. Warm, a debug build answers in about 0.007 s, so the budget is
#: far from both answers.
BUDGET_SECONDS = 2.0


@pytest.mark.backlog_fixed("RP-077")
def test_psql_describe_collation_subquery(pg: Any) -> None:
    """PostgreSQL 15.19 answers `[('v', 'C')]` (measured on a private
    cluster: the scenario creates a table) in milliseconds, because the
    subquery's equalities on the outer row pick one pg_collation row and one
    pg_type row. Evaluating the 911 x 181 cross product is the same answer,
    a minute later.

    The first run after the `CREATE TABLE` is not timed: it pays for
    rebuilding the catalog after DDL (RP-715). Only decoded catalog rows and
    plans are cached, never a result, so the second run still evaluates the
    subquery for every column."""
    pg.execute('CREATE TABLE rp_cx (v text COLLATE "C")')
    assert pg.execute(COLLATION_QUERY).fetchall() == [("v", "C")]
    start = time.monotonic()
    rows = pg.execute(COLLATION_QUERY).fetchall()
    elapsed = time.monotonic() - start
    assert rows == [("v", "C")]
    assert elapsed < BUDGET_SECONDS, f"{elapsed:.1f} s for one column"


def _first_and_warm(pg: Any, prefix: str) -> tuple[float, float]:
    """The fastest of three first runs of the collation query, each straight
    after a `CREATE TABLE`, and the fastest of the warm runs after them."""
    firsts, warms = [], []
    for i in range(3):
        pg.execute(f'CREATE TABLE {prefix}_{i} (v text COLLATE "C")')
        query = COLLATION_QUERY.replace("'rp_cx'", f"'{prefix}_{i}'")
        start = time.monotonic()
        assert pg.execute(query).fetchall() == [("v", "C")]
        firsts.append(time.monotonic() - start)
        for _ in range(2):
            start = time.monotonic()
            pg.execute(query).fetchall()
            warms.append(time.monotonic() - start)
    return min(firsts), min(warms)


@pytest.mark.backlog_fixed("RP-715")
def test_the_first_catalog_query_after_ddl_costs_what_the_next_does(pg: Any) -> None:
    """PostgreSQL 15.19 (private cluster, 2026-11-09) answers the collation
    query in 0.9 ms straight after a `CREATE TABLE` and 0.2 ms after that:
    DDL invalidates only the cache entries it changed (`inval.c`). A debug
    `secantusd-pg` took 0.58 s and then 0.016 s, because any DDL rebuilt
    `pg_attribute` whole, PostgreSQL's own 3,054 rows with it, and built it
    twice (the query, and the walk that counts its catalog reads). Since
    those rows are built once per process, a debug build takes about 0.02 s
    and then 0.007 s (2026-11-12).

    A ratio, not a budget, so the verdict holds on any runner and any build:
    the fastest first-after-DDL run of three against the fastest warm run."""
    first, warm = _first_and_warm(pg, "rp715")
    assert first < 3 * warm + 0.02, (
        f"first query after DDL {first * 1000:.0f} ms, warm {warm * 1000:.0f} ms"
    )


@pytest.fixture
def pg_alone(tmp_path: Path) -> Iterator[Any]:
    """A connection to a `secantusd-pg` of the test's own, whose schema no
    other test's tables enlarge."""
    from tests.test_rust_pgserver_slice import _Server

    with _Server(tmp_path) as server, server.connect() as conn:
        yield conn


#: User tables added between the two measurements.
GROWTH_TABLES = 400


@pytest.mark.backlog_fixed("RP-715")
def test_the_first_catalog_query_after_ddl_does_not_grow_with_the_schema(
    pg_alone: Any,
) -> None:
    """PostgreSQL 15.19 answers the first collation query after a `CREATE
    TABLE` in the same time however many other tables there are (0.33 ms
    with 200, 0.20 ms with 400, private cluster, 2026-11-12): its scans read
    the one relation's rows by index, and DDL invalidates only the cache
    entries it changed (`inval.c`). A debug `secantusd-pg` took about
    0.010 s with no other table, 0.049 s with 400 and 0.092 s with 800
    (2026-11-12): a read of one type's `pg_type` row built every type's row,
    the catalog-read walk counted every `pg_attribute` row for a sequential
    scan PostgreSQL does not make (RP-909), and each catalog version
    re-decoded every table definition, read each table's index list and
    copied the definitions several times over. Since then a debug build
    takes about 0.011 s with none and 0.023 s with 400.

    The fastest first-after-DDL run with 400 more tables against the
    fastest with none, in one server."""
    small, _ = _first_and_warm(pg_alone, "rp715a")
    for i in range(GROWTH_TABLES):
        pg_alone.execute(f"CREATE TABLE rp715_grow_{i} (a int, b text)")
    large, _ = _first_and_warm(pg_alone, "rp715b")
    assert large < 2 * small + 0.01, (
        f"first query after DDL {small * 1000:.0f} ms with no other table, "
        f"{large * 1000:.0f} ms with {GROWTH_TABLES}"
    )


@pytest.mark.backlog("RP-911")
def test_the_first_catalog_query_after_ddl_costs_the_same_at_800_tables(
    pg_alone: Any,
) -> None:
    """PostgreSQL 15.19 answers the first collation query after a `CREATE
    TABLE` in the same time however many other tables there are (0.33 ms
    with 200, 0.20 ms with 400, private cluster, 2026-11-12). A debug
    `secantusd-pg` takes about 0.011 s with no other table and 0.036 s with
    800 (2026-11-12): the statement after any DDL republishes the session's
    per-version catalog state (`install_session_state`) from every table.

    The fastest first-after-DDL run with 800 more tables against the
    fastest with none, in one server, within half again plus 5 ms."""
    small, _ = _first_and_warm(pg_alone, "rp911a")
    for i in range(2 * GROWTH_TABLES):
        pg_alone.execute(f"CREATE TABLE rp911_grow_{i} (a int, b text)")
    large, _ = _first_and_warm(pg_alone, "rp911b")
    assert large < 1.5 * small + 0.005, (
        f"first query after DDL {small * 1000:.0f} ms with no other table, "
        f"{large * 1000:.0f} ms with {2 * GROWTH_TABLES}"
    )
