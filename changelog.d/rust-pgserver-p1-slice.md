### A PostgreSQL server written in Rust, sharing one database with the Python one

SecantusDB already speaks the PostgreSQL wire protocol, and it already has a
Rust server for the MongoDB side. This is the first slice of the third: a
PostgreSQL server written entirely in Rust, with no Python anywhere in the
request path. It handles `CREATE TABLE`, `INSERT` and single-table `SELECT`
today — a deliberately thin slice, because the point of it is to prove the
architecture end to end rather than to be useful yet.

The part that matters is that the two servers share one database. A table
created by the Rust server can be read and written by the Python server, and a
table created by the Python server can be read by the Rust one, because both
write the same catalog documents into the same WiredTiger store. That contract
is pinned by golden vectors captured from the Python server, since a catalog
written subtly wrong by one server is not an error the other reports — it is a
table with the wrong columns.

SQL is parsed by PostgreSQL's own parser, statically linked, rather than by a
general-purpose SQL parser. That is a correctness decision as much as a
performance one: the shapes SecantusDB currently has to work around, like
`COPY … WITH (freeze on)`, parse correctly by construction. Anything the new
server cannot yet handle answers the PostgreSQL error code for "feature not
supported" rather than guessing, so an unsupported query is always a refusal
and never a wrong answer.

#### Added

- `secantus-pgcatalog`, `secantus-pgplan` and `secantus-pgserver` crates, plus
  the standalone `secantusd-pg` binary.
- `invoke rust-pgserver-build` and `invoke rust-pgserver-test`; the latter is
  now part of `invoke rust-gate`.
- Cross-server round-trip tests covering both directions, PostgreSQL-oracle
  checked predicates, and the error codes for unknown columns, unknown tables,
  duplicate tables, duplicate keys and unsupported constructs.

#### Fixed

- A duplicate primary key reported MongoDB's `E11000 duplicate key error`
  through the PostgreSQL connection. It now reports what PostgreSQL reports,
  down to the `DETAIL: Key (id)=(1) already exists.` line.
