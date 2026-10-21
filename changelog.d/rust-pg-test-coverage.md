### The Rust PostgreSQL server's differential tests now gate every pull request

Most of what checked the Rust PostgreSQL server against a real PostgreSQL ran
only on a developer's machine. The 320 differential corpora, the 836-case
differential suite, more than a hundred regression tests left by fixed backlog
items, most of the server's own Rust tests and all of its pgwire fork's tests
ran in no CI lane at all, so a change that broke what any of them pinned could
merge under a green check.

They all run on every pull request now. PostgreSQL's answers to every corpus
line are recorded beside the corpora, so the corpus test needs no PostgreSQL
in CI; the differential suite, the backlog's regression and gap tests and
every `cargo test` target run in the lane that builds the server.

#### Added

- `tests/test_rust_pg_corpora.py` holds `secantusd-pg` to PostgreSQL's
  recorded answers for 319 corpora (14,687 checks, in
  `tools/probes/pg_corpora/expected/`), each on a server of its own. A check
  whose answer reads the clock or the server process (86) is compared on
  status and column types only; every other check is compared exactly. A
  divergence the server has must be listed with the open backlog item that
  owns it, and the list fails when an entry goes stale. Today it is empty:
  every corpus matches.
- `tools/probes/pg_corpora_sweep.py --record DIR` and
  `tools/probes/pg_corpora_expect.py` make and refresh those records: two
  recordings on fresh PostgreSQL 14 and 15 clusters, compared with each other.
- CI (`pg-oracle`): the whole differential suite (one of its 14 tests ran
  before), the corpus test, the `backlog_fixed` regression tests of the Rust
  PG backlog against a PostgreSQL 15 the job starts, and
  `tools/backlog.py verify --file backlog-rust-postgres.md`.
- CI (`rust`): fmt, clippy `-D warnings` and the 156 tests of
  `secantus-pgwire`.
- `tests/test_ci_runs_rust_server_tests.py` also requires every test file
  that needs the `secantusd-pg` binary to be named, as a whole file, by a
  workflow, and no longer counts a mention in a YAML comment as running it.

#### Fixed

- CI ran three of `secantus-pg`'s cargo test targets; it now runs all of them
  (the crate's 101 unit tests, `tls`, `connection_addrs`, `declare_reads` and
  the doctests were in no lane).
- `pg_differential.py` ran `COPY ... TO STDOUT` through `execute`, which left
  the connection mid-command on both servers, so the 13 checks after it in
  `matviews.sql` compared two identical client errors and measured nothing.
  COPY now runs through `cursor.copy()` and its output is compared, and a
  statement that leaves the connection mid-command stops the probe.
- `pg_corpora_sweep.py` counted a probe that printed its summary and then
  crashed as a clean corpus; it is now DID-NOT-FINISH.
- `secantus-pgwire`: two clippy errors in SecantusDB's own patches
  (`BackendMessageTranscoder`'s type, `IdleEvent::Fatal` now boxes its
  `ErrorInfo`), formatting, and the TLS SNI test that was `#[ignore]`d: it
  dropped its client stream before the server finished the handshake.
- `./inv rust-pgserver-test` (and so `rust-gate`) gates `secantus-pgwire` as
  CI does, and `./inv rust-fmt` formats it and `secantus-pg`.
- A cancel that arrives while a large statement is being PLANNED is
  answered at once (RP-002's regression test,
  `tests/backlog/test_rp_triage.py::test_cancel_during_parse_is_honoured_at_once`,
  failed the first time CI ran it). Only the parse was a cancellation point,
  and the parse is memoised, so every repeat of a statement, and every
  statement on a machine that parses faster than planning takes, waited out
  the whole plan: a cancel 0.5 s into the test's 200,000-item `IN` list was
  answered 0.85 s late on a debug build. Planning and the pre-planning passes
  (privileges, the plan-cache key) now check between passes, as
  PostgreSQL's `CHECK_FOR_INTERRUPTS` does: the worst case measured is
  0.15 s on a debug build, against PostgreSQL 15.19's 0.03 s.
- The corpus records were made on macOS, where `cot(1)` ends `...308`; on
  glibc, PostgreSQL and this server both print `...306` (both compute
  `1.0 / tan(x)`). The test accepts the other platform's answer for that
  check, listed with its reason.
- The PostgreSQL test fixtures delete each store once its server exits: a
  store preallocates a 128 MB WiredTiger log, and 1,100 of them filled a
  CI runner's disk.

#### Found

- RP-077: psql's `\d <table>` column query takes 64 s on a debug build and
  4.3 s on a release build (PostgreSQL: 3.5 ms), all of it in a correlated subquery over a comma join
  of `pg_collation` and `pg_type` that evaluates the whole cross product.
  The corpus test showed it the first time it ran `psql_describe` (268 s).
  Gap test: `tests/backlog/test_rp_catalog_latency.py`.
