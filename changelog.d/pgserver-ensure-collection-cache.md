### Seven session opens per statement, for catalogs that were already there

Profiling the Rust PostgreSQL server found that a `SELECT 1` — a statement that
touches no table at all — cost fifty-five microseconds more than an empty
protocol round trip, against PostgreSQL's nine. The call tree put most of that
somewhere unexpected: before running anything, every statement opened seven
fresh WiredTiger sessions, each with a cursor to open, search and close, to
re-confirm that the seven catalog collections still existed. They are created
once and never dropped except with the whole database.

The check itself has to stay. One catalog write — the row recording a new
table — has no guard of its own and relies on that blanket pass; removing it
would turn a `CREATE TABLE` in a fresh database into a storage error rather
than a no-op. What can go is the repetition. A connection that has established
a collection exists cannot subsequently be wrong about it, so the verdict is
now remembered for the life of the connection.

Deliberately per connection rather than process-wide: the test suite builds
many servers over many storage paths that all call their database `postgres`,
and a process-wide verdict would cheerfully report a collection as present in a
store that had never seen one.

A `SELECT 1` now costs 55.9 microseconds where it cost 76.3, and a single-row
primary-key read 68.0 where it cost 89.2 — about a fifth off every statement,
with the conformance suite unchanged at 5542 passing.

#### Changed

- `ensure_collection` on the Rust PostgreSQL server caches its verdict per
  connection instead of re-probing storage before every statement.
