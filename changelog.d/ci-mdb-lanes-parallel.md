### CI for the Rust MongoDB server runs in parallel lanes

A pull request that touched only the Rust MongoDB server waited 24 to 26
minutes for CI, set by two serial jobs. `storage-engine` built the wheel and
then, one after another, ran the smoke tests, built a debug binary and
compared that binary with a live mongod, none of which needed the wheel.
`rust-storage` linted and tested five WiredTiger-linked crates in sequence,
and its cargo cache, cleaned against seven workspaces at once, kept almost
nothing: a full-match restore still compiled about 350 crates.

The mongod comparisons now run in their own job, beside the wheel build
instead of after it, `rust-storage` runs as five lanes, and the smoke tests
run under xdist. The slowest MongoDB lane of a pull request now takes 10.9
minutes (run 43070100764, warm caches), against 24 to 26 before: `storage-engine`
10.9, `rust-storage` 3.3 to 7.4 per lane, `mdb-binary` 5.0 and 7.3. A
full-match restore in a `rust-storage` lane compiles 10 crates, not 652.

#### Changed

- `test.yml` `rust-storage`: five parallel lanes (`storage`,
  `storage-async`, `adapter`, `mdb`, `pg`). Every gate the serial job ran
  runs in exactly one lane. Each lane's rust-cache names one workspace root:
  rust-cache cleans the target once per listed workspace, so seven roots
  over one shared target kept only the intersection of their dependencies,
  7 of 583 packages.
- New `mdb-binary` job, two shards: `backlog` (`tools/backlog.py verify`
  for the RM items) and `live` (`tests/mongod_live/` and the R7 binary
  tests). Both build the debug `secantusd-rs` against the cached
  WiredTiger build the `rust-storage` lanes use.
- `storage-engine`: the smoke-test step runs with `-n auto
  --dist=loadgroup`; the R7 step stays here for the macOS cron cell only.
- `storage-engine`'s Windows cell runs on the weekly cron and on a manual
  dispatch only, no longer on every push to `main`, where it took 55 to 66
  minutes against ~11 for every other MongoDB lane. A Windows-only break now
  shows up at the next weekly run, a dispatch
  (`gh workflow run test.yml --ref <branch>`) or a release build.

#### Fixed

- `tests/test_rust_binary_smoke.py`: every server the file starts has its
  stderr read on a thread from spawn, and its stdout once the banner is
  read. At `slowms: 0` the test's server wrote 4310 bytes of `Slow query`
  lines to a stderr nobody read until it exited, past a Windows pipe's
  ~4 KiB, so it blocked in a log write and pymongo cancelled the
  operation: the Windows cell of `storage-engine` failed on every push to
  `main` from 2026-11-10.
