### The Rust PostgreSQL server: autovacuum

The server now vacuums and analyzes tables in the background, as PostgreSQL
15.19's autovacuum does, with no statement asking. A launcher wakes every
`autovacuum_naptime`, shared out among the databases. Its worker vacuums
and analyzes each table whose dead, inserted or changed tuples pass the
`autovacuum_*` thresholds: the base threshold plus the scale factor times
`reltuples`, from the GUCs or the table's own storage parameters. The
results show in `last_autovacuum`, `autovacuum_count`, `last_autoanalyze`
and `autoanalyze_count`.

#### Added

- `secantus-pg`: `autovacuum.rs`, the launcher and its worker
  (`autovacuum.c`). Fixes RP-226
  (`tests/backlog/test_rp_batch79.py::test_autovacuum_runs_on_its_own`).
  - A reload that changes the naptime reaches the launcher's sleep.
  - The worker's session takes the configuration file's and the
    database's settings, as a backend's start does.
  - A table another session holds a conflicting lock on is skipped, as the
    worker's conditional lock skips it.
  - Temporary tables and partitioned parents are left alone.
  - Its VACUUM and ANALYZE are reported under the autovacuum counters.
  - The server's stop joins the launcher before the store closes.
