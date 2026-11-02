### Rust PG server: DROP DATABASE refuses a database other sessions are on

DROP DATABASE dropped a database out from under the sessions connected to
it. It now waits for them to leave, as PostgreSQL's `CountOtherDBBackends`
does, and otherwise fails with 55006 `database "d" is being accessed by
other users` and its session-count detail.

#### Fixed

- Fixes RP-344 (tests/backlog/test_rp_other_backends_wait.py::test_drop_database_refuses_while_another_session_is_on_it)
