### The login probes take an ephemeral port

The three login comparison probes started their private PostgreSQL 15.19
reference cluster on a fixed port, so two gates running at once collided.

#### Fixed

- `tools/probes/pg_login_tls_files.py` (port 5418), `pg_login_hba.py`
  (5419) and `pg_login_options.py` (5417) now each ask the kernel for a free
  port, so two worktrees running `scripts/gate_pg.sh` at the same time no
  longer fail the second cluster with "could not bind IPv4 address: Address
  already in use".

Fixes RP-345 (tests/test_rust_pg_login_differential.py::test_logins_answer_as_postgresql_does)
