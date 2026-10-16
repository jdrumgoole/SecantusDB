### Rust PostgreSQL server: a user there is no role for is refused by default

PostgreSQL never lets in a role that does not exist. The Rust PostgreSQL
server used to trust one when started without a `pg_hba.conf`, which a
test could come to depend on without noticing. It now refuses it, as
PostgreSQL does (Joe, 2026-10-16).

#### Changed

- **A user name there is no role for is refused by default**: it is asked
  for a password, as PostgreSQL asks one under a `scram-sha-256` line, and
  none it gives is right. A role with no password, `postgres` among them,
  still connects without one. Fixes the last of RP-013's login defaults
  (tests/backlog/test_rust_postgres_gaps.py::test_unknown_user_is_asked_for_a_password).
- **`--trust-unknown-users`** (`ServerOptions::trust_unknown_users`,
  `PgBuilder::trust_unknown_users`, `trust_unknown_users=True` on the
  Python handle) lets such a user in without a password, as before. It
  cannot be given beside `--hba-file`, whose lines decide it.
- `--refuse-unknown-users` and `refuse_unknown_users` are still accepted
  and change nothing; the Rust methods are deprecated.

**To upgrade**: a test that connects as a role nobody created (`user=test`,
or no user at all, which libpq sends as the operating-system user name)
now fails to log in. Connect as `postgres`, create the role first, or start
the server with `--trust-unknown-users`.
