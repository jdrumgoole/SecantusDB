### A skipped test now says why it skipped

Six test suites diff SecantusDB's SQL against a live PostgreSQL. When that
server is unreachable they skip — and until now they skipped with the words
`no local PostgreSQL oracle`, which is indistinguishable from *PostgreSQL is
not installed*. A suite disabled by a transient connection failure looked
exactly like a suite deliberately switched off, so nobody could tell whether
~109 tests were reporting green because they passed or because they never ran.
Answering that question required a full 20-minute suite run.

The six suites now share one probe, `tests/pg_oracle.py`, and its skips name
the DSN and the underlying exception. They also stop drifting: the copies had
grown three different default DSNs between them, one of which omitted the user.

For the record, the suites are not skipping — a full run executes all 195 of
those tests. The backlog entry claiming otherwise described a Postgres.app
permission gate on a box that runs Homebrew's PostgreSQL, and has been
corrected. The documented location of this machine's `mongod` builds was
corrected the same way: the reference-server notes named a version that is not
installed and called two that are "gone".

#### Fixed

- The PostgreSQL reference-server suites (`test_sql_search_path`,
  `test_sql_subms_timestamps`, `test_sql_operator_types`,
  `test_sql_result_type_tags`, `test_sql_float_rendering`,
  `test_sql_isolation_level`) skip with the DSN and the connection error
  instead of a bare "no local PostgreSQL oracle".
- Three drifted default DSNs collapsed into one; the probe is cached, so a
  worker opens one connection rather than nine. That bounds collection-time
  cost at one `connect_timeout` when the server hangs rather than refuses.

#### Changed

- `CLAUDE.md` and `tasks/remaining-work-plan.md` record the three `mongod`
  builds actually installed (6.0.16, 8.2.11 on `PATH`, 8.3.4) with `Cellar`
  paths, replacing claims that `PATH` gives 8.2.1 — which is installed nowhere
  — and that 6.0.16 and 8.3.4 "are gone". Version-difference probing described
  by older entries is reproducible on this box today.
- A docstring in `tests/test_rust_pgserver_differential.py` asserted that the
  old `skipif` shape leaked a connection per worker. Measured: it does not —
  CPython collects the unreferenced connection immediately. The claim is
  corrected rather than repeated.
