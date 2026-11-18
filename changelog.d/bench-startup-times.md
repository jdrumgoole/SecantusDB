### Startup-time benchmark for both server pairs

`invoke startup-times` now compares `mongod` with `secantusd-mdb` and
PostgreSQL with `secantusd-pg`, for cold and warm starts, and keeps a history.

#### Changed

- `bench/startup_times.py` measures four servers. `mongod` is timed both as
  a one-member replica set and standalone, and PostgreSQL's cold start
  includes `initdb`. The deprecated Python server is no longer measured.
- `--record` appends each run to `bench/results/startup_history.jsonl`, with
  the date, host, versions and commit.
