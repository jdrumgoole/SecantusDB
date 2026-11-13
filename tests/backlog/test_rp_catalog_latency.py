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


def _first_and_warm(pg: Any, prefix: str, rounds: int = 3) -> tuple[float, float]:
    """The fastest of `rounds` first runs of the collation query, each
    straight after a `CREATE TABLE`, and the fastest of the warm runs after
    them."""
    firsts, warms = [], []
    for i in range(rounds):
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


@pytest.fixture
def pg_pair(tmp_path: Path) -> Iterator[tuple[Any, Any]]:
    """Connections to two `secantusd-pg`s of the test's own: one schema
    stays empty while the test grows the other."""
    from tests.test_rust_pgserver_slice import _Server

    (tmp_path / "small").mkdir()
    (tmp_path / "large").mkdir()
    with (
        _Server(tmp_path / "small") as small_server,
        _Server(tmp_path / "large") as large_server,
        small_server.connect() as small,
        large_server.connect() as large,
    ):
        yield small, large


def _alternating_firsts(
    small: Any, large: Any, prefix: str, rounds: int = 5
) -> tuple[float, float]:
    """The fastest of `rounds` first runs of the collation query straight
    after a `CREATE TABLE` on each connection, the two taking turns: runs
    taken one server after the other met the machine's load at different
    times, and a load that rose between them failed a server that had
    not slowed (2026-11-13)."""
    firsts: dict[str, list[float]] = {"small": [], "large": []}
    for i in range(rounds):
        for side, conn in (("small", small), ("large", large)):
            table = f"{prefix}_{side}_{i}"
            conn.execute(f'CREATE TABLE {table} (v text COLLATE "C")')
            query = COLLATION_QUERY.replace("'rp_cx'", f"'{table}'")
            start = time.monotonic()
            assert conn.execute(query).fetchall() == [("v", "C")]
            firsts[side].append(time.monotonic() - start)
    return min(firsts["small"]), min(firsts["large"])


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


@pytest.mark.backlog_fixed("RP-911")
def test_the_first_catalog_query_after_ddl_costs_the_same_at_800_tables(
    pg_pair: tuple[Any, Any],
) -> None:
    """PostgreSQL 15.19 answers the first collation query after a `CREATE
    TABLE` in the same time however many other tables there are (0.33 ms
    with 200, 0.20 ms with 400, private cluster, 2026-11-12). A debug
    `secantusd-pg` took about 0.011 s with no other table and 0.036 s with
    800, and after three passes over the republication (2026-11-12 and
    -13) about 0.010 s and 0.018 s. Since the planner derives the row
    types, the plain tables' `tableoid` and the publishable relations from
    the documents they were published with only when a statement looks one
    up, a `pg_catalog` type's name is answered without them, and a keyed
    `pg_attribute` read decodes its own relation's composite alone, about
    0.010 s and 0.014 s (2026-11-13).

    The fastest of five first-after-DDL runs on a server with 800 more
    tables against the fastest on one with none, taking turns, within half
    again plus 2 ms. The bound was half again plus 5 ms until 2026-11-13,
    when the gap shrank to about 12 ms and 5 ms fell inside the runs'
    noise. A quarter again plus 2 ms, tried the same day, passed a debug
    build here (10.1 ms and 13.7 ms) only through its 2 ms, and failed CI,
    whose runs are about three times slower (29 ms and 41 ms, PR #2716):
    the ratio is about 1.4 on both, so the absolute term is kept small.
    Until the same day the two were measured one after the other on one
    server, and a machine loaded by another build failed it twice in six.
    What is left of the growth is the next test's."""
    small_conn, large_conn = pg_pair
    for i in range(2 * GROWTH_TABLES):
        large_conn.execute(f"CREATE TABLE rp911_grow_{i} (a int, b text)")
    small, large = _alternating_firsts(small_conn, large_conn, "rp911a")
    assert large < 1.5 * small + 0.002, (
        f"first query after DDL {small * 1000:.0f} ms with no other table, "
        f"{large * 1000:.0f} ms with {2 * GROWTH_TABLES}"
    )


@pytest.mark.backlog("RP-911")
def test_the_first_catalog_query_after_ddl_costs_the_same_at_800_tables_within_a_tenth(
    pg_pair: tuple[Any, Any],
) -> None:
    """PostgreSQL 15.19 answers the first collation query after a `CREATE
    TABLE` in the same time however many other tables there are (0.33 ms
    with 200, 0.20 ms with 400, private cluster, 2026-11-12): DDL
    invalidates only the cache entries of the rows it changed (`inval.c`).
    A debug `secantusd-pg` takes about 0.011 s with no other table and
    0.015 s with 800 (2026-11-13, with the constraint texts and comments
    derived at their first lookup): the statement after any DDL still
    reads every table's and every row type's catalog document, each
    table's index list and the relations for `regclass`, for each catalog
    version, and its own catalog reads cost about 1 ms more.

    The fastest of five first-after-DDL runs on a server with 800 more
    tables against the fastest on one with none, taking turns, within a
    tenth again plus 1 ms: the growth the previous test's bound still
    allows (about 3.5 ms) fails it by more than a millisecond."""
    small_conn, large_conn = pg_pair
    for i in range(2 * GROWTH_TABLES):
        large_conn.execute(f"CREATE TABLE rp911_grow_{i} (a int, b text)")
    small, large = _alternating_firsts(small_conn, large_conn, "rp911c")
    assert large < 1.1 * small + 0.001, (
        f"first query after DDL {small * 1000:.1f} ms with no other table, "
        f"{large * 1000:.1f} ms with {2 * GROWTH_TABLES}"
    )


def _fastest_create(pg: Any, prefix: str) -> float:
    """The fastest of five `CREATE TABLE`s."""
    times = []
    for i in range(5):
        start = time.monotonic()
        pg.execute(f'CREATE TABLE {prefix}_{i} (v text COLLATE "C")')
        times.append(time.monotonic() - start)
    return min(times)


@pytest.mark.backlog_fixed("RP-921")
def test_create_table_costs_the_same_at_400_tables(pg_alone: Any) -> None:
    """PostgreSQL 15.19 runs a `CREATE TABLE` in the same time however many
    other tables there are (0.41 ms with none, 0.37 ms with 800, private
    cluster, 2026-11-13). A debug `secantusd-pg` took about 1.4 ms with
    none and 42 ms with 800 (2026-11-13): after the table was made, its new
    indexes' page models were looked for (`btree_built`: the hash, other
    access-method and B-tree shapes), and each look listed every table's
    index relations from the whole catalog, read and decoded afresh while
    the statement ran; its check that no index already had the name
    decoded the whole catalog too. With both reading only what they need,
    1.2 ms and 5.9 ms (RP-928 holds the rest).

    The fastest of five with 400 more tables against the fastest with none,
    in one server, within half again plus 5 ms."""
    small = _fastest_create(pg_alone, "rp_ct_a")
    for i in range(GROWTH_TABLES):
        pg_alone.execute(f"CREATE TABLE rp_ct_grow_{i} (a int, b text)")
    large = _fastest_create(pg_alone, "rp_ct_b")
    assert large < 1.5 * small + 0.005, (
        f"CREATE TABLE {small * 1000:.1f} ms with no other table, "
        f"{large * 1000:.1f} ms with {GROWTH_TABLES}"
    )


def _fastest_first_over_two_types(pg: Any, prefix: str) -> float:
    """The fastest of five first runs of the collation query straight after
    a `CREATE TABLE` of an `int` and a `text` column."""
    times = []
    for i in range(5):
        table = f"{prefix}_{i}"
        pg.execute(f"CREATE TABLE {table} (a int, b text)")
        query = COLLATION_QUERY.replace("'rp_cx'", f"'{table}'")
        start = time.monotonic()
        assert pg.execute(query).fetchall() == [("a", None), ("b", None)]
        times.append(time.monotonic() - start)
    return min(times)


@pytest.mark.backlog("RP-927")
def test_the_first_catalog_query_after_ddl_over_two_column_types_costs_the_same_at_400_tables(
    pg_alone: Any,
) -> None:
    """PostgreSQL 15.19 answers the collation query for a table of an `int`
    and a `text` column, straight after its `CREATE TABLE`, in the same time
    however many other tables there are (0.35 ms with none, 0.33 ms with
    800, private cluster, 2026-11-13): the subquery reads one `pg_type` row
    per column through the type oid's index. A debug `secantusd-pg` took
    about 0.016 s with none and 0.10 s with 800 (2026-11-13): one keyed
    build per catalog per message cycle, so the second column's type, a
    second key in the cycle, builds every `pg_type` row, each table's row
    type with its owner from all of `pg_class`.

    The fastest of five with 400 more tables against the fastest with none,
    in one server, within half again plus 5 ms."""
    small = _fastest_first_over_two_types(pg_alone, "rp_tt_a")
    for i in range(GROWTH_TABLES):
        pg_alone.execute(f"CREATE TABLE rp_tt_grow_{i} (a int, b text)")
    large = _fastest_first_over_two_types(pg_alone, "rp_tt_b")
    assert large < 1.5 * small + 0.005, (
        f"first query after DDL {small * 1000:.1f} ms with no other table, "
        f"{large * 1000:.1f} ms with {GROWTH_TABLES}"
    )


@pytest.mark.backlog("RP-928")
def test_create_table_costs_the_same_at_800_tables_within_a_tenth(pg_alone: Any) -> None:
    """PostgreSQL 15.19 runs a `CREATE TABLE` in the same time however many
    other tables there are (0.41 ms with none, 0.37 ms with 800, private
    cluster, 2026-11-13): it finds a clashing relation by name through
    `pg_class`'s name index (`get_relname_relid`). Once its index builds
    stopped reading every table (RP-921), a debug `secantusd-pg` took about
    1.2 ms with none and 5.9 ms with 800 (2026-11-13): its checks that the
    name is free list every composite type and every table's indexes.

    The fastest of five with 800 more tables against the fastest with none,
    in one server, within a tenth again plus 1 ms: the growth the previous
    test's bound still allows fails it."""
    small = _fastest_create(pg_alone, "rp_ct8_a")
    for i in range(2 * GROWTH_TABLES):
        pg_alone.execute(f"CREATE TABLE rp_ct8_grow_{i} (a int, b text)")
    large = _fastest_create(pg_alone, "rp_ct8_b")
    assert large < 1.1 * small + 0.001, (
        f"CREATE TABLE {small * 1000:.1f} ms with no other table, "
        f"{large * 1000:.1f} ms with {2 * GROWTH_TABLES}"
    )
