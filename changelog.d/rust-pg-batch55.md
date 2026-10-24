### Rust PostgreSQL server: the cumulative statistics

The `pg_stat_*` and `pg_statio_*` views answer, from counters kept the way
PostgreSQL 15.19 keeps them.

#### Added

- The cumulative statistics (RP-070; decided by Joe: model the counters,
  and a buffer cache for the block counts). Each of the 21 views is
  PostgreSQL's own definition, over its `pg_stat_get_*` functions:
  - `pg_stat_*_tables`, `pg_stat_*_indexes` and `pg_stat_xact_*`;
  - `pg_stat_*_functions` and `pg_stat_database`;
  - `pg_statio_*`.

  The counters, and how they move:
  - **Writes.** A session counts its inserts, updates (heap-only ones
    apart), deletes and TRUNCATEs in the transaction level they happen in,
    savepoints included. They fold in as PostgreSQL's `AtEOXact` does:
    every attempt counts, and a commit makes inserts live and updates and
    deletes dead tuples, an abort its inserts and updates dead.
  - **Scans.** A sequential scan counts the table's visible tuples; an
    index scan counts its index entries and the heap tuples it fetched.
    Each follows the scan the storage planner runs, as `EXPLAIN` shows it.
    Index builds count as sequential scans, as PostgreSQL's do.
  - **Flushing.** The counts reach the shared ones as the session goes
    idle: at once when a second has passed since its last flush, else
    after ten seconds idle or when `pg_stat_force_next_flush()` asks. A
    reader keeps what it first read until its transaction ends
    (`stats_fetch_consistency`, and `pg_stat_clear_snapshot()`), and the
    `pg_stat_xact_*` views show the session's own unflushed counts.
  - **VACUUM and ANALYZE** report their live and dead tuples straight to
    the shared counts. `pg_stat_reset()` and the single-table and
    single-function resets work.
  - **Functions** are counted when `track_functions` asks: calls, and
    total and own time.
  - **Databases** count transactions, tuples, deadlocks, sessions and
    their times. A session killed by `pg_terminate_backend` or abandoned
    by its client is counted as such.
  - **Block counts** come from a model of the shared buffer pool: every
    page a scan, a fetch or a write touches, a read the first time since
    start and a hit while it stays in `shared_buffers`.
  - **Persistence.** The shared counts are written at a clean shutdown and
    read back once at startup. A crash loses them, as PostgreSQL's are
    lost.

  Fixes RP-070:
  - tests/backlog/test_rp_found_2026_10_20.py::test_catalog_relations_are_queryable
  - tests/backlog/test_rp_batch55.py::test_scenario[a table's tuple counters]
  - tests/backlog/test_rp_batch55.py::test_scenario[an aborted transaction's tuples are dead]
  - tests/backlog/test_rp_batch55.py::test_scenario[a transaction's own counts]
  - tests/backlog/test_rp_batch55.py::test_scenario[an index's scans]
  - tests/backlog/test_rp_batch55.py::test_scenario[VACUUM, ANALYZE and TRUNCATE]
  - tests/backlog/test_rp_batch55.py::test_scenario[a reader keeps its first values for the transaction]
  - tests/backlog/test_rp_batch55.py::test_scenario[a table's counters reset]
  - tests/backlog/test_rp_batch55.py::test_scenario[a database's transactions and tuples]
  - tests/backlog/test_rp_batch55.py::test_scenario[a function's calls, tracked when asked]
  - tests/backlog/test_rp_batch55.py::test_scenario[a table's pages are read, then hit]
  - tests/backlog/test_rp_batch55.py::test_the_counters_outlive_a_clean_restart

#### Fixed

- A session whose client goes away without a Terminate is counted in
  `sessions_abandoned`. A terminated one is counted in `sessions_killed`
  as its FATAL goes out, not when its client closes the socket. Fixes
  RP-227 (tests/backlog/test_rp_batch55.py::test_sessions_abandoned_and_killed).
  The vendored pgwire gains a `client_gone` hook for it.

#### Known

- RP-224, filed: a sequential scan under LIMIT counts every tuple.
  PostgreSQL stops counting where the scan stops.
- RP-225, filed: the system catalogs' counters stay at zero.
- RP-226, filed: autovacuum is not modelled.
