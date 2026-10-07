### The psycopg gauge now runs the Rust PostgreSQL server on Windows, in CI

psycopg 3's own test suite had never finished against `secantusd-pg` on
Windows. A new workflow builds the server on `windows-latest` and runs the
gauge there, so that result now comes from a real run. The run completes,
and every test it starts reports a result.

#### Added

- `.github/workflows/psycopg-windows.yml`: builds WiredTiger and a debug
  `secantusd-pg` on `windows-latest`, then runs
  `SECANTUS_GAUGE_SERVER=rust python -m psycopg_validation.runner`. The job
  fails when the run is truncated, meaning the tests that started outnumber
  the ones that reported a result. It runs weekly, on demand, and on PRs that
  touch `crates/secantus-pg*`, `psycopg_validation/` or the workflow itself.
- `SECANTUS_PSYCOPG_GAUGE_TIMEOUT` raises the runner's wall-clock cap on
  slower hosts.

#### Fixed

- The Windows gauge stopped at about 65% of the run. One test,
  `test_type_error_shadow`, takes 30 s against PostgreSQL itself on that
  runner, which is more than the gauge's 20 s limit per test. On Windows,
  pytest-timeout can only use its thread method, so a test that runs over the
  limit ends the whole pytest process. The test is now deselected on Windows,
  with that reason written down.
- Two `test_right_exception_on_session_timeout` tests are deselected on
  Windows. They expect PostgreSQL's Windows-only behaviour, where an abortive
  close destroys the server's FATAL. `secantusd-pg` closes the connection
  gracefully and delivers the real `25P03`.
- The Windows runner image exports `PGPASSWORD`. That made `test_used_password`
  expect a password challenge that a trusted role never gets. The variable is
  now unset for the gauge step.
- psycopg's `refcount` marker is now excluded on Windows, as psycopg's own
  scheduled Windows CI does. These are client-side checks that count live
  Python objects to find leaks. On the runner they passed in one run and
  failed 75 `test_leak` cases in the next, some with negative counts.
