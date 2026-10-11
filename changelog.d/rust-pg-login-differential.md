### Rust PostgreSQL server: the login comparisons run in CI

The login rules of the Rust PostgreSQL server were measured against
PostgreSQL 15 by scripts that lived outside the repository, so nothing
would have failed had a later change moved them.

#### Added

- `tools/probes/pg_login_options.py`, `pg_login_tls_files.py` and
  `pg_login_hba.py`: 817 logins put to a throwaway PostgreSQL 15 cluster
  and to `secantusd-pg`, each printing how many answer the same.
- `tests/test_rust_pg_login_differential.py` runs the three and requires
  every scenario to match, and each probe to hold the number of scenarios
  it is known to. It needs `SECANTUS_PG15_BIN`; CI's `pg-oracle` lane
  installs PostgreSQL 15 and sets it.
- `SECANTUS_GAUGE_TLS=1` runs psycopg's suite with the Rust server started
  with a certificate and libpq told `sslmode=verify-full`. Measured on
  2026-10-12: 5,544 pass and none fail of the 5,731 that ran, the same
  counts as the plain run.
