### Rust PG server: CREATE DATABASE ... TEMPLATE and ALTER DATABASE ... RENAME wait for other sessions to leave

A session that had just hung up but whose backend had not yet exited failed
these statements with 55006. They now wait as PostgreSQL 15.19's
`CountOtherDBBackends` does -- up to 50 counts, 100 ms apart -- and a cancel
ends the wait.

#### Fixed

- `create database d template s` and `alter database s rename to t` succeed
  when the other sessions on `s` leave within the wait, instead of failing
  at once.

Fixes RP-343 (tests/backlog/test_rp_other_backends_wait.py::test_create_database_waits_for_a_closing_template_session)
