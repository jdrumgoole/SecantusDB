### Rust PostgreSQL server: citext, and the PostgreSQL gauges can drive it

#### Added

- `CREATE EXTENSION citext` on the Rust PG server, matching PostgreSQL 15. citext columns and casts compare, order, group, dedup and enforce `UNIQUE` / `PRIMARY KEY` case-insensitively.
  - `LIKE` / `~` on citext are case-insensitive, and citext's own string functions (`strpos`, `replace`, `split_part`, the regexp family) match case-insensitively.
  - citext against a typed `text` value compares as text, as PostgreSQL resolves it.
  - Corpus `citext`: 79 lines, 0 divergences.
- The SQLAlchemy, pgjdbc, pgx and pgtest gauges can run against the Rust server through a shared `pg_gauge_server.py` switch.
  - Set `SECANTUS_GAUGE_SERVER=rust`. Results go to a `-rust-server` report beside the Python server's, and a binary built from a different source tree is refused.
  - The SQLAlchemy dialect suite, run against the Rust server for the first time, found the schema gap now in the backlog.

#### Fixed

- A JOIN `ON` condition over a nondeterministic collation now matches case-insensitively. It used to match only rows with the same case.
- A grouped nondeterministic-collation column used inside an expression no longer raises 42803.
