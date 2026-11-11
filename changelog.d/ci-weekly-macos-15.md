### The weekly macOS build runs again, on macos-15

GitHub retired the `macos-14` runner image, so the weekly cron's macOS
`storage-engine` cell stopped starting ("The configuration
'macos-14-arm64-us-default' is not supported"): macOS went untested from
2026-11-09. The cell now runs on `macos-15`, the runner the release binaries
already build on.

#### Changed

- The `storage-engine` job's weekly and dispatch matrix uses `macos-15`.

#### Fixed

- Eight test files that start a Rust server now keep reading its output
  after the listening banner, instead of leaving it in a pipe nobody reads
  until the server stops. A full pipe (64 KiB on Linux and macOS, about
  4 KiB on Windows) blocks every server thread that writes a line -- the
  deadlock that failed `test_rust_binary_smoke.py` on Windows. MongoDB
  server: `test_rust_binary_pitr.py`, `test_crash_recovery.py`. PostgreSQL
  server: `test_rust_pg_binary_smoke.py`, `test_rust_pgserver_slice.py`
  (both of its server launchers), `test_rust_pg_corpora.py`,
  `test_rust_pg_builtin_coverage.py`, `test_rust_pg_dollar_names.py`,
  `test_rust_pg_rule_trees.py`. The shared helper is `tests/server_pipes.py`.
