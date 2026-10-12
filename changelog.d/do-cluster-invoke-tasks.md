### The droplet benchmark: a task for every command, and one for PostgreSQL

#### Added

- `invoke do-destroy` and `invoke do-ssh`, and `--server-size` /
  `--client-size` on `invoke do-up`. Every `do-cluster` command now has a
  task.
- `invoke do-pg-bench` (`bench/do_pg_bench.py`): the measurement behind the
  site's Rust PostgreSQL table, a released `secantusd-pg` against PostgreSQL
  16 on the server droplet. It was done by hand, and so had not been
  repeated since 0.1.0-beta.7. The raw trials are kept under
  `bench/results/pg-droplet/`.
