### Rust PostgreSQL server: the WAL, file and configuration-file views answer

As decided (Joe, 2026-10-22): `pg_current_wal_lsn()` and
`pg_relation_filepath()` answer from WiredTiger -- the log position is
WiredTiger's, a relation's file the WiredTiger file holding it -- and
`pg_hba_file_rules` / `pg_ident_file_mappings` show the files the server was
started with, `pg_config` its own build.

#### Added

- `pg_current_wal_lsn()` (and `pg_current_wal_insert_lsn` /
  `pg_current_wal_flush_lsn`): WiredTiger's newest log record as an LSN --
  its log file's number the high half, its offset the low -- read through a
  `log:` cursor that resumes where the last read stopped. It only moves
  forward, and moves when a write commits.
- `pg_walfile_name(lsn)` (PostgreSQL's 16 MB segment naming) and
  `pg_wal_lsn_diff(a, b)`.
- `pg_relation_filepath(rel)`: the WiredTiger file holding a table's,
  index's or sequence's rows, relative to the data directory; NULL for a
  view or a catalog this server computes.
- `secantus-wt`: a `log:` cursor's `III` key; `secantus-storage`:
  `Storage::log_position`. (Both are shared with the MongoDB server, which
  does not call them.)

- `pg_hba_file_rules` and `pg_ident_file_mappings`: the files given with
  `--hba-file` / `--ident-file`, read again at query time as PostgreSQL
  reads them, by the tokenizer the logins use (no rows without one);
  `pg_file_settings`: no rows, as no `postgresql.conf` is read; `pg_config`:
  PostgreSQL's 23 names, `VERSION` and `BINDIR` and the Rust build's
  `CONFIGURE` / `CC` / `CFLAGS` filled, the rest empty.

#### Fixed

- Fixes RP-112 (`tests/backlog/test_rp_batch26.py::test_wal_and_file_functions`):
  the functions were refused (0A000).
- Fixes RP-101 (`tests/backlog/test_rp_batch26.py::test_configuration_file_views`):
  the four views were refused (0A000); `pg_hba_file_rules` matches
  PostgreSQL 15.19's for the same file, line for line.
- Fixes RP-132 (`hba::tests::a_list_continues_past_blanks_after_its_comma`):
  a `pg_hba.conf` list with blanks after a comma (`all,  test`) ended at
  the comma, so the next field was read in its place and the server refused
  to start; PostgreSQL's tokenizer skips them.
- Fixes RP-129 (`tests/backlog/test_rp_batch26.py::test_statement_timeout_is_prompt`):
  `statement_timeout` ended a long statement seconds late -- a 1 s timeout
  answered 57014 after 14.6 s on a 50-million-row `generate_series` and
  after 10.5 s on a 36-million-pair join (release build; PostgreSQL 15.19:
  1.0 s). The planner's cancellation hook now reads the statement's
  deadline as well as the cancel flag, and says which one stopped it
  (`statement timeout` / `user request`); the series and join loops check
  every 65,536 rows, and both cases now stop at the deadline.
- Fixes RP-060 (`tests/backlog/test_rp_found_2026_10_20.py::test_infinite_timestamps`,
  `tests/backlog/test_rp_batch26.py::test_scenario[infinite timestamps in every operator]`):
  a `timestamp` / `timestamptz` comparison dropped `infinity` and
  `-infinity` (`t > '2020-01-01'` lost the infinite row, an index's
  `BETWEEN '-infinity' AND 'infinity'` the finite one); they are ordered as
  PostgreSQL orders its extreme values, against an instant and against each
  other. `infinity` plus or minus an interval (or a date plus days) is
  itself, an infinite difference is 22008 `cannot subtract infinite
  timestamps` / `dates`, `extract` gives +/-Infinity for the unbounded units
  and NULL for the rest, and `date_trunc` keeps it.

#### Performance

- pg_dump of an empty database took 84 s (debug build): its function query
  asks, for each of the 3,244 built-ins, `EXISTS (... FROM pg_transform
  WHERE ... AND (p.oid = trffromsql OR p.oid = trftosql))`, which re-planned
  per row. A correlated subquery whose WHERE, without its outer references,
  admits no row at all is now answered once for every outer row; pg_dump
  takes 4.5 s.
- A catalog relation's rows are computed once per message cycle, so a
  correlated subquery over one does not rebuild it per outer row.
