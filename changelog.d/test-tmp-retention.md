### Tests keep one run's temp directory, and reap the Rust tests' stores

A test run no longer keeps the three runs before it on disk, and the
WiredTiger homes `cargo test` leaves in the temp directory are reclaimed.

#### Fixed

- **pytest keeps one numbered run directory, not three**
  (`tmp_path_retention_count = 1` in `pyproject.toml`). A run's exit
  deletes the runs before it. Each server-starting test leaves a
  WiredTiger home of about 260 MB on Linux, so one CI step's run holds tens
  of GB: on 2026-11-07 the `storage-engine` job on a 72 GB runner held 38 GB
  of earlier steps' runs, and the RP-312 disk watchdog failed PR #2408. The
  retention policy stays `all`: nothing is deleted from a run that is still
  going.
- **The Rust tests' temp stores are reaped.** About sixty Rust test files
  make a WiredTiger home per test, named `secantus-<kind>-<pid>-<n>`, and
  never delete it (1,166 of them, 2.5 GiB, on one Mac). pytest's session
  start and end, and `invoke clean`, now delete one once the process its
  name gives is gone and it has sat untouched for half an hour
  (`python_tasks._sweep_stale_rust_test_tmp`; tests in
  `tests/test_clean_pytest_tmp.py`, one of which checks every such name in
  `crates/` against the sweep).
