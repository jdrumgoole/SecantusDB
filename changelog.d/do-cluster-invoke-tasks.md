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
- That measurement runs its clients on the two client droplets, over the
  private network (`bench/pg_remote_client.py`). It used to run them beside
  the server, where a lone client and the server share CPUs: one trial of
  one or four clients then moved by up to 21%.

#### Changed

- `bench/pg_concurrency.py` checks that the server binary is current when
  it starts the server, where it used to check on import.
