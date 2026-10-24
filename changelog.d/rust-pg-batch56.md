### Rust PostgreSQL server: COPY streams its rows, and commands report their progress

COPY FROM loads its rows as their data arrives. The `pg_stat_progress_*`
views answer while a command runs.

#### Added

- The progress views (RP-100; decided by Joe: stream COPY, a progress slot
  for every command). Each backend has PostgreSQL's progress slot (a
  command, its relation and twenty parameters), which a running COPY,
  VACUUM, VACUUM FULL, ANALYZE, CLUSTER or CREATE INDEX fills.
  `pg_stat_get_progress_info` lists the slots, and the six views are
  PostgreSQL 15.19's definitions over it. `pg_stat_progress_basebackup`
  stays empty, as no backend here runs a base backup.
- COPY FROM parses and loads each CopyData message's whole rows when the
  message arrives, as PostgreSQL's `CopyFrom` does; it no longer buffers
  everything until CopyDone. A row split across messages loads when its
  end comes, in text, CSV (a quoted field may hold a newline) and binary
  formats. Data after a `\.` line is ignored. Outside a block the COPY is
  one transaction from its first rows to CopyDone, and a CopyFail or a bad
  row rolls it back.
- Another session reading `pg_stat_progress_copy` mid-COPY sees PostgreSQL's
  values: `bytes_processed` the bytes received, `tuples_processed` the rows
  loaded, `COPY FROM` and `PIPE`.

Fixes RP-100 (tests/backlog/test_rp_batch26.py::test_scenario[progress views],
tests/backlog/test_rp_batch56.py::test_copy_progress,
tests/backlog/test_rp_batch56.py::test_copy_rows_split_across_messages,
tests/backlog/test_rp_batch56.py::test_progress_view_shapes,
tests/backlog/test_rp_found_2026_10_20.py::test_catalog_relations_are_queryable).
