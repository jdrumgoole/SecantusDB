### Rust pgserver: extensions (hstore, postgis) and a role catalog

The Rust PG server now takes `CREATE EXTENSION` for the two extensions the
psycopg test-suite reaches for. `hstore` brings the key/value map type with
PostgreSQL's operator set — `->`, `?`, `?|`, `?&`, `@>`, `<@`, `||`, `-` —
and `postgis` brings a `geometry` type whose EWKB round-trips through
`ST_GeomFromEWKB` / `ST_AsEWKB` and shapely. Each extension registers its
type under the oid a client discovers through `pg_type` / `pg_extension`,
and `DROP EXTENSION` removes it again; forty-three gauge tests that skipped
on "extension unavailable" now run.

Roles landed alongside: `CREATE / ALTER / DROP ROLE` (and the `USER`
spellings) keep a cluster-wide catalog. `pg_roles` and `pg_user` mask every
password as `********`, `pg_authid` carries the SCRAM-SHA-256 verifier — the
one derived from a plaintext `PASSWORD` (4096 iterations, a fresh 16-byte
salt, exactly what `password_encryption = scram-sha-256` produces) or the one
libpq's `PQchangePassword` sends ready-made — and every error and notice was
measured on PostgreSQL 16: `42710`, `42704` and the `IF EXISTS` notice,
`2BP01` for the bootstrap superuser, `55006` for the session's own user,
`22007` for a bad `VALID UNTIL`, the empty-password notice, and a multi-name
`DROP ROLE` that is all or nothing. Passwords are recorded, never checked:
every connection is still trusted, which is what keeps the gauges connecting
as `user=postgres` with no password.

#### Added
- `secantus-pgplan` / `secantus-pgserver`: `CREATE EXTENSION [IF NOT EXISTS]`
  / `DROP EXTENSION [IF EXISTS]` for `hstore` and `postgis`, the
  `pg_extension` catalog, the `hstore` type and operators (`hstore.rs`), the
  `geometry` type over EWKB (`geometry.rs`).
- `secantus-pgserver`: `CREATE / ALTER / DROP ROLE|USER` with a persisted
  role catalog; `pg_roles`, `pg_authid`, `pg_user` virtual tables;
  SCRAM-SHA-256 verifier derivation via `secantus-auth`.

#### Fixed
- `secantus-pgplan`: a stored `timestamptz` column's `::text` dropped the
  offset and rendered in UTC regardless of the session zone — the cast chain
  now carries the column's declared type, so `t::text` renders
  `2026-01-01 13:00:00+01` under `Europe/Berlin` as PostgreSQL does.
- `secantus-pgplan`: `ALTER ROLE ... PASSWORD $1` is the `42601` syntax error
  PostgreSQL raises (its grammar takes only a string constant there) instead
  of silently clearing the password.
