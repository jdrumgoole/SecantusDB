### Rust PostgreSQL server: prepared INSERTs with UUID keys no longer re-plan every row

A prepared statement executed again (an `executemany`, a driver's hot path)
did work per row that its answer never depended on. Each Describe and Execute
parsed the statement text again to ask whether it was a FETCH, and a
statement with a `uuid` parameter was planned from scratch on every execution
because the plan cache took only integers, doubles and strings. psycopg's
`test_type_error_shadow`, 150,000 UUID-keyed INSERTs from three connections,
went from 18.7 s to 11.6 s in a debug build (PostgreSQL 15.19: 8 s). It is the
test that hit psycopg's 20 s limit on every Linux CI run.

#### Fixed

- `secantus-pg`: `check_cached_result_type` parses the statement only when
  the catalog has moved since it was described, not on every Describe and
  Execute (RP-014, part).
- `secantus-pg` (`plan_cache.rs`): a parameter declared `uuid` is templated.
  Bind has already made it canonical, or refused it with 22P02, so the
  template stores and matches exactly what planning would. Pinned against
  PostgreSQL 15.19 by
  `tests/test_rust_pgserver_slice.py::test_a_reused_plan_over_a_uuid_parameter_stores_it_canonical`.
- `secantus-pgplan`: `canonical_type_ref` is memoised, and the custom range
  and multirange lookups return at once when the session has none.
- `tests/test_detached_run.py::test_child_survives_a_kill_of_the_launching_group`
  wrote its marker with `echo done > marker`, which creates the file before
  it writes, so a poll between the two read an empty file and failed. The
  marker is now renamed into place.
