### The Rust PostgreSQL server scales with readers

Two point-read clients against the Rust PostgreSQL server used to get LESS
done together than one did alone: 21,000 statements a second for one client
fell to 14,400 for two. Each statement re-read the whole user catalog and
copied every session setting whenever a worker thread switched from one
connection to another, because both per-thread caches were keyed by the
connection. They are now keyed by what they actually depend on, and the same
benchmark scales like PostgreSQL 15 does: 1.81x at two clients (PostgreSQL
1.86x) and 3.6x at eight (PostgreSQL 3.6x).

#### Fixed

- `secantus-pgserver`: the planner's per-session catalog tables are shared
  between connections with the same role, database and session user; a
  session with its own temporary tables or functions, or a database with
  row-level security, keeps a private copy.
- `secantus-pgserver` / `secantus-pgplan`: session settings reach the planner
  as a shared reference instead of a full copy per statement.
- `secantus-pgserver`: `pg_get_serial_sequence` on a table that does not exist
  raises `42P01`, and on a column that does not exist `42703`, as PostgreSQL
  does; both used to answer NULL.
- psycopg gauge: on Windows it deselects the `proxy`, `timing` and `mypy`
  markers, as psycopg's own Windows CI does, and clears a crash flag left by
  an earlier interrupted run that otherwise stopped every later run.
- `bench/pg_concurrency.py` honours `SECANTUSD_PG`, so a worktree's build can be
  measured.
