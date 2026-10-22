### CI: the pg-oracle job runs as two shards

With the Windows cell off pull requests, `pg-oracle` set a PR's CI time: 19 to
27 minutes, 11 to 18 of them one pytest step over six files on a 4-core
runner. Its default `loadgroup` scheduling left three workers idle at the end
(4m44 on the last 24 tests of run 39749683550), behind a corpus test that
takes three minutes on its own.

#### Changed

- `test.yml` `pg-oracle` is a matrix of two shards. `server` runs the slice
  suite, builds `secantusd-rs`, and keeps the cargo tests, the login
  comparisons and the backlog tests. `differential` runs everything compared
  with PostgreSQL's answers: the differential suite against the PostgreSQL 14
  service, and the binary-only files (the corpora, the built-in coverage, the
  dollar names and the binary smoke test). A file named in neither list runs
  nowhere.
- Both shards run pytest with `--dist worksteal` (no file in the lane uses
  `xdist_group`) and `--durations=40`, so the next long test is named in the
  log.
