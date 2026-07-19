### The admin console catches up with three servers

The admin console was written when SecantusDB had one server. Since then a
Rust server and a PostgreSQL-wire SQL server shipped, and the console
quietly fell behind: a hardcoded table of "what the Rust server can't do
yet" went stale within days of being written and spent months hiding six
feature groups — archive restore, oplog and TTL pruning, role
grant/revoke, `killOp`, the server log, and profiling — behind disabled
buttons, on a server that implements every one of them.

That table is gone. Only a real `mongod` keeps a static capability
profile, because its negatives are definitional rather than a snapshot of
a moving target: no `mongod` will ever serve the proprietary
`secantusAdmin.*` commands. Both SecantusDB servers now start fully
permissive, and a feature is withdrawn only when the target itself
answers `CommandNotFound` — negative knowledge learned from the live
server instead of guessed in advance, so the console cannot drift out of
step with either server again.

The same review closed the rest of the gap. Point-in-time recovery, the
largest shipped subsystem with no interface at all, gets a panel on the
backup page. The embedded-server button can start the Rust server, not
just the Python one. The role picker asks the connected target what roles
exist rather than consulting the Python server's own table. Collections
keyed by `Decimal128`, `UUID`, or `Binary` `_id` can be browsed at last,
and a tampered pagination cursor now returns a clean error instead of an
unhandled crash.

#### Added

- Point-in-time recovery on `/backup`: take a base snapshot, and recover
  an archive to a wall-clock moment into a fresh directory.
- The embedded-server control can launch either the Python or the Rust
  server; the picker appears when the Rust extension is installed.
- Collation input on the index-create form, and a collation badge in the
  index list.
- `Decimal128`, `UUID`, and `Binary` `_id` values are supported by the
  collection browser's pagination cursor, `Binary` round-tripping its
  subtype.

#### Changed

- Capability detection no longer keeps a per-flavour feature table for
  SecantusDB servers. Features are hidden only after the target reports
  `CommandNotFound`.
- The role picker and `/roles` catalogue are sourced from the connected
  target via `rolesInfo`, falling back to the built-in names. Role names
  submitted from the form are no longer filtered against a local table,
  so a valid custom role is no longer silently discarded.

#### Fixed

- `~/.secantus/admin.db`, which stores target URIs verbatim including
  credentials, is created `0600` with its directory `0700`. Previously it
  was left at the process umask while the token file beside it was
  already locked down.
- A malformed pagination cursor returns a `ValueError`-shaped error
  rather than an unhandled `InvalidId` / `InvalidOperation` from a bson
  constructor.
- Pointing the console at a `postgresql://` URI explains that the SQL
  server has no admin UI, instead of failing with an opaque pymongo
  parse error.
