### Every pull request now compares the Rust MongoDB server with mongod

The tests that compare the Rust MongoDB server with a real mongod existed,
but almost none of them ran in CI. The 260 regression test cases that pin fixed
backlog items were excluded from every pytest run and ran only on a
developer's machine, and no CI job installed mongod after the Python servers
were deprecated. Several other files ran nowhere either: the standalone
binary's point-in-time-restore tests, the `secantusdb` crate's own tests
(including the `secantus_mdb::Server` embedding API), and four files gated on
the engine extension.

Every pull request now installs mongod 8.2 and runs the MongoDB backlog's
gap and regression tests, plus a new set of live comparisons (`tests/mongod_live/`)
for surface no backlog item covered. A new crash test SIGKILLs the binary in
the middle of concurrent `j: true` writes in its default mode. The new
comparisons found six divergences: two are fixed here, and four are filed
with gap tests.

#### Added

- `.github/workflows/test.yml`: the storage-engine job installs
  `mongodb-org-server` 8.2 on Linux and runs `tools/backlog.py verify --file
  backlog-rust-mongodb.md --regressions` and `tests/mongod_live/`; its
  standalone-binary step also runs `test_rust_binary_pitr.py` and the new
  `test_rust_binary_crash_durability.py`. The rust-storage job runs `cargo
  test` for `secantusdb` and `secantus-wt`. The rust job builds
  `_secantus_core` and runs `test_rust_group_field_pushdown.py` and the Rust
  halves of `test_tolower_timestamp_local_time.py`,
  `test_decimal128_special_values.py` and `test_decimal128_zero_and_avg.py`.
- `tools/backlog.py verify --regressions`: with `--file`, also run that
  server's regression tests (`SECANTUS_BACKLOG_PREFIXES` limits them to its
  ID prefix, so the MongoDB run needs no PostgreSQL).
- `tests/mongod_live/` (marker `mongod_live`, outside the default suite):
  window functions and `$fill`, change-stream pre-images,
  `startAtOperationTime` and split events, retryable-write retries, and an
  operator and command tail, 75 cases against a live mongod.
- `tests/test_rust_binary_crash_durability.py`: four concurrent clients
  insert, update and delete with `{w: 1, j: true}`; after a SIGKILL and
  restart every document holds its last acknowledged value. With `j: false`
  it reports about 150 lost writes, so it has teeth.
- `tests/mongod_reference.py`: the mongod launcher, moved out of the
  deprecated `test_mongod_differential.py` so the Rust tests no longer import
  the Python server to start their reference.

#### Fixed

- `$shift` answered null when its `output` evaluated to MISSING on the
  shifted row; mongod 8.2.11 answers the `default`. Fixes RM-227
  (`tests/backlog/test_rm_found_2026_10_21.py::test_shift_default_replaces_a_missing_output`).
- The server ran `collmod`, a lowercase alias mongod 8.2 does not have (59
  CommandNotFound). Pinned by
  `tests/mongod_live/test_operator_tail.py::test_command_matches_mongod[collmod]`.
- RM-185's lock-count regression tests failed under parallel load: mongod's
  asynchronous tail work from the previous case landed in the next case's
  window. Each case now waits for the counts to settle before it starts.
- `tests/test_ci_runs_rust_server_tests.py` now also catches files gated on
  `_secantus_core` or the binary (`SECANTUSDB_BIN`), and records why each
  retired parity suite does not run.

#### Found

Filed with gap tests in `tests/backlog/test_rm_found_2026_10_21.py`:

- RM-225: `$expMovingAvg` answers code 2 for a null, missing or Decimal128
  input.
- RM-226: window specs mongod refuses with its own code (605001, 9, 5339902,
  5429414) answer code 2.
- RM-228: `$convert` to a target it cannot produce answers code 2 and
  ignores `onError`.
- RM-229: a change stream's post-image is the document as it is when the
  event is read, not as the update left it.
