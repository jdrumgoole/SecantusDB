"""Gap tests for how long the Rust PG server takes over catalog queries real
clients send (`tasks/backlog-rust-postgres.md`, "Found by the corpus test").

Found on 2026-10-21, when `tests/test_rust_pg_corpora.py` first ran every
corpus: `psql_describe` took 268 s, 263 of them in four runs of the column
query psql sends for `\\d <table>`.
"""

from __future__ import annotations

import statistics
import time
from collections.abc import Callable, Iterator
from pathlib import Path
from typing import Any

import pytest

from tests.backlog.conftest import only_on

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


#: Rounds each paired timing takes.
ROUNDS = 15


def _paired(
    small: Any, large: Any, run: Callable[[Any, str], float], prefix: str
) -> list[tuple[float, float]]:
    """`ROUNDS` timings of `run` on each connection, taken in pairs back to
    back so the machine's load meets both alike: a minimum taken over each
    server's runs apart let a load that rose or fell between them flip the
    verdict either way (2026-11-13; CI on 2026-11-14, -15 and -17)."""
    pairs = []
    for i in range(ROUNDS):
        pairs.append((run(small, f"{prefix}_s_{i}"), run(large, f"{prefix}_l_{i}")))
    return pairs


def _median_excess(pairs: list[tuple[float, float]], factor: float) -> float:
    """The median over the rounds of how far the large server's run went
    past `factor` times its pair's: one noisy round, on either side, cannot
    move it."""
    return statistics.median(large - factor * small for small, large in pairs)


def _describe(pairs: list[tuple[float, float]], what: str, tables: int) -> str:
    small = statistics.median(s for s, _ in pairs)
    large = statistics.median(big for _, big in pairs)
    return (
        f"{what}: median {small * 1000:.1f} ms with no other table, "
        f"{large * 1000:.1f} ms with {tables}"
    )


def _ratios(pairs: list[tuple[float, float]], what: str, tables: int) -> str:
    """Each round's large-over-small ratio and the median, for the log of a
    run that passes as well as one that fails."""
    ratios = sorted(large / small for small, large in pairs)
    return (
        f"{_describe(pairs, what, tables)}; ratio median "
        f"{statistics.median(ratios):.2f}, rounds {' '.join(f'{r:.2f}' for r in ratios)}"
    )


def _first_collation(conn: Any, table: str) -> float:
    """The collation query's first run straight after a `CREATE TABLE`."""
    conn.execute(f'CREATE TABLE {table} (v text COLLATE "C")')
    query = COLLATION_QUERY.replace("'rp_cx'", f"'{table}'")
    start = time.monotonic()
    assert conn.execute(query).fetchall() == [("v", "C")]
    return time.monotonic() - start


def _grow(conn: Any, prefix: str, tables: int) -> None:
    for i in range(tables):
        conn.execute(f"CREATE TABLE {prefix}_grow_{i} (a int, b text)")


@pytest.mark.backlog_fixed("RP-715")
def test_the_first_catalog_query_after_ddl_does_not_grow_with_the_schema(
    pg_pair: tuple[Any, Any],
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

    A server with 400 more tables against one with none, within twice
    plus 10 ms.

    Measured as 15 paired rounds (`_paired`), the median of the large
    server's run less twice times its pair's under 10 ms (2026-11-17: the
    minima of each server's runs taken apart let CI's noise flip verdicts,
    PRs #2732, #2744, #2769 and #2794)."""
    small, large = pg_pair
    _grow(large, "rp715", GROWTH_TABLES)
    pairs = _paired(small, large, _first_collation, "rp715a")
    assert _median_excess(pairs, 2) < 0.01, _describe(pairs, "first query after DDL", GROWTH_TABLES)


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

    The first-after-DDL run on a server with 800 more tables against one
    with none, within
    twice plus 2 ms: the growth this guards against was more than three
    times (0.011 s against 0.036 s), and what is left of it is the next
    test's. Half again plus 2 ms failed CI on 2026-11-14 at 24 ms and
    39 ms (PR #2732, a ratio of 1.62) and passed the same commits on a
    rerun: CI's ratio moves between about 1.4 and 1.6 from run to run, so
    a bound at 1.5 measured the runner's noise. The bound was half again
    plus 5 ms until 2026-11-13, when the gap shrank to about 12 ms and 5 ms fell inside the runs'
    noise. A quarter again plus 2 ms, tried the same day, passed a debug
    build here (10.1 ms and 13.7 ms) only through its 2 ms, and failed CI,
    whose runs are about three times slower (29 ms and 41 ms, PR #2716):
    the ratio is about 1.4 on both, so the absolute term is kept small.
    Until the same day the two were measured one after the other on one
    server, and a machine loaded by another build failed it twice in six.

    Measured as 15 paired rounds (`_paired`), the median of the large
    server's run less twice times its pair's under 2 ms (2026-11-17: the
    minima of each server's runs taken apart let CI's noise flip verdicts,
    PRs #2732, #2744, #2769 and #2794)."""
    small, large = pg_pair
    _grow(large, "rp911", 2 * GROWTH_TABLES)
    pairs = _paired(small, large, _first_collation, "rp911a")
    assert _median_excess(pairs, 2) < 0.002, _describe(
        pairs, "first query after DDL", 2 * GROWTH_TABLES
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

    The first-after-DDL run on a server with 800 more tables against one
    with none, within a
    tenth again plus 1 ms: the growth the previous test's bound still
    allows (about 3.5 ms) fails it by more than a millisecond.

    Measured as 15 paired rounds (`_paired`), the median of the large
    server's run less 1.1 times its pair's under 1 ms (2026-11-17: the
    minima of each server's runs taken apart let CI's noise flip verdicts,
    PRs #2732, #2744, #2769 and #2794).
    The fastest of five passed CI on 2026-11-17 (PR #2794) while a debug
    build here measured a ratio of 1.23 to 1.29 in ten of ten runs of
    fifteen rounds, by minimum and by median alike."""
    only_on(
        "darwin",
        why="the same debug build passes on x86_64 Linux CI (PRs #2794 and "
        "#2821, min-of-five and paired-median methods) and fails every run "
        "on arm64 macOS at a median ratio of 1.26-1.27 (2026-11-17)",
    )
    small, large = pg_pair
    _grow(large, "rp911", 2 * GROWTH_TABLES)
    pairs = _paired(small, large, _first_collation, "rp911c")
    print(_ratios(pairs, "first query after DDL", 2 * GROWTH_TABLES))
    assert _median_excess(pairs, 1.1) < 0.001, _describe(
        pairs, "first query after DDL", 2 * GROWTH_TABLES
    )


def _create(conn: Any, table: str) -> float:
    """One `CREATE TABLE`."""
    start = time.monotonic()
    conn.execute(f'CREATE TABLE {table} (v text COLLATE "C")')
    return time.monotonic() - start


@pytest.mark.backlog_fixed("RP-921")
def test_create_table_costs_the_same_at_400_tables(pg_pair: tuple[Any, Any]) -> None:
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

    A server with 400 more tables against one with none, within
    four times plus 5 ms: the growth this guards
    against was about thirty times. Half again plus 5 ms failed CI on
    2026-11-14 at 2.6 ms and 9.7 ms (PR #2744) and passed the same commits
    on a rerun: CI runs about three times slower than a laptop, so the
    residual growth RP-928 holds already fills most of a fixed 5 ms
    there.

    Measured as 15 paired rounds (`_paired`), the median of the large
    server's run less four times its pair's under 5 ms (2026-11-17: the
    minima of each server's runs taken apart let CI's noise flip verdicts,
    PRs #2732, #2744, #2769 and #2794)."""
    small, large = pg_pair
    _grow(large, "rp_ct", GROWTH_TABLES)
    pairs = _paired(small, large, _create, "rp_ct")
    assert _median_excess(pairs, 4) < 0.005, _describe(pairs, "CREATE TABLE", GROWTH_TABLES)


def _first_over_columns(columns: str) -> Callable[[Any, str], float]:
    """The collation query's first run straight after a `CREATE TABLE` of
    two columns `a` and `b` of `columns`, neither with a collation."""

    def run(conn: Any, table: str) -> float:
        conn.execute(f"CREATE TABLE {table} ({columns})")
        query = COLLATION_QUERY.replace("'rp_cx'", f"'{table}'")
        start = time.monotonic()
        assert conn.execute(query).fetchall() == [("a", None), ("b", None)]
        return time.monotonic() - start

    return run


@pytest.mark.backlog_fixed("RP-927")
def test_the_first_catalog_query_after_ddl_over_two_column_types_costs_the_same_at_400_tables(
    pg_pair: tuple[Any, Any],
) -> None:
    """PostgreSQL 15.19 answers the collation query for a table of an `int`
    and a `text` column, straight after its `CREATE TABLE`, in the same time
    however many other tables there are (0.35 ms with none, 0.33 ms with
    800, private cluster, 2026-11-13): the subquery reads one `pg_type` row
    per column through the type oid's index. Before RP-927 a debug `secantusd-pg` took
    about 0.016 s with none and 0.10 s with 800 (2026-11-13): one keyed
    build per catalog per message cycle, so the second column's type, a
    second key in the cycle, builds every `pg_type` row, each table's row
    type with its owner from all of `pg_class`.

    The fastest of five with 400 more tables against the fastest with none,
    within half again plus 5 ms.

    Measured as 15 paired rounds (`_paired`), the median of the large
    server's run less 1.5 times its pair's under 5 ms (2026-11-17: the
    minima of each server's runs taken apart let CI's noise flip verdicts,
    PRs #2732, #2744, #2769 and #2794)."""
    small, large = pg_pair
    _grow(large, "rp_tt", GROWTH_TABLES)
    pairs = _paired(small, large, _first_over_columns("a int, b text"), "rp_tt")
    assert _median_excess(pairs, 1.5) < 0.005, _describe(
        pairs, "first query after DDL", GROWTH_TABLES
    )


@pytest.mark.backlog_fixed("RP-928")
def test_create_table_costs_the_same_at_800_tables_within_a_tenth(
    pg_pair: tuple[Any, Any],
) -> None:
    """PostgreSQL 15.19 runs a `CREATE TABLE` in the same time however many
    other tables there are (0.41 ms with none, 0.37 ms with 800, private
    cluster, 2026-11-13): it finds a clashing relation by name through
    `pg_class`'s name index (`get_relname_relid`). Once its index builds
    stopped reading every table (RP-921), a debug `secantusd-pg` took about
    1.2 ms with none and 5.9 ms with 800 (2026-11-13): its checks that the
    name is free list every composite type and every table's indexes.
    Those became lookups by name (2026-11-14), and the last pass over every
    relation, the hash-index check's map of every relation's oid rebuilt
    after the statement's own catalog write, became a lookup of the one
    relation's key (2026-11-15: 2.9 ms to 1.6 ms with 800, nineteen of
    twenty runs alone within the bound at a load average near 13).

    A server with 800 more tables against one with none, within
    twice plus 2 ms. As the gap test it held a tenth
    again plus 1 ms, which measured the growth then left (about 5x); kept as
    the regression bound once RP-928 closed, it failed CI on 2026-11-15 at
    1.8 ms and 3.2 ms (rm snapshot PR #2769, a shared-storage change that
    does not scale with tables): a millisecond under CI load is noise.
    Twice plus 2 ms still catches a return of that growth.

    Measured as 15 paired rounds (`_paired`), the median of the large
    server's run less twice times its pair's under 2 ms (2026-11-17: the
    minima of each server's runs taken apart let CI's noise flip verdicts,
    PRs #2732, #2744, #2769 and #2794)."""
    small, large = pg_pair
    _grow(large, "rp_ct8", 2 * GROWTH_TABLES)
    pairs = _paired(small, large, _create, "rp_ct8")
    assert _median_excess(pairs, 2) < 0.002, _describe(pairs, "CREATE TABLE", 2 * GROWTH_TABLES)


@pytest.mark.backlog_fixed("RP-970")
def test_the_first_catalog_query_after_ddl_over_two_user_types_costs_the_same_at_400_tables(
    pg_pair: tuple[Any, Any],
) -> None:
    """PostgreSQL 15.19 answers the collation query for a table of two
    columns of two user enums, straight after its `CREATE TABLE`, in the
    same time however many other tables there are (0.15 ms with none, 0.22
    ms with 400, private cluster, 2026-11-14): one `pg_type` row per column
    through the oid index. A debug `secantusd-pg` took 22 ms with none and
    79 ms with 400 (2026-11-14): a key of a user type built every user
    type, and the second one in a message cycle built every row. Since
    RP-970 a user type's key builds that type's row alone.

    The bound is RP-927's: a server with 400 more tables against one with
    none, within half again plus 5 ms.

    Measured as 15 paired rounds (`_paired`), the median of the large
    server's run less 1.5 times its pair's under 5 ms (2026-11-17: the
    minima of each server's runs taken apart let CI's noise flip verdicts,
    PRs #2732, #2744, #2769 and #2794)."""
    small, large = pg_pair
    for conn in pg_pair:
        conn.execute("CREATE TYPE rp_e1 AS ENUM ('x')")
        conn.execute("CREATE TYPE rp_e2 AS ENUM ('y')")
    _grow(large, "rp_te", GROWTH_TABLES)
    pairs = _paired(small, large, _first_over_columns("a rp_e1, b rp_e2"), "rp_te")
    assert _median_excess(pairs, 1.5) < 0.005, _describe(
        pairs, "first query after DDL", GROWTH_TABLES
    )
