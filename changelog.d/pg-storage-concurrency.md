### The Rust PostgreSQL server now scales with clients, and a statement costs a third less

Under concurrent clients the Rust PostgreSQL server used to plateau at four
connections: SELECT throughput went from 11k to 24k operations a second between
one client and eight, where PostgreSQL 16 on the same box went from 27k to 83k.
The cause was not a lock. Each connection ran as a tokio task, and every
statement — a blocking WiredTiger call — went through `block_in_place`, which
hands the task's worker off and back on every statement; at eight connections
the runtime spent more time shuffling workers than serving statements. Each
connection now gets its own OS thread (with its own single-threaded runtime),
the shape PostgreSQL has one level up with its process per connection. SELECT
now reaches 70k operations a second at eight clients, 3.1x the single-client
rate and the same scaling PostgreSQL shows; INSERT is now faster than
PostgreSQL at every client count (55k against 45k at eight).

The single-client cost came down too, by removing work that was repeated on
every statement without ever changing its answer: the reply was written to the
socket message by message instead of once per Sync; the parsed statement was
cloned out of the parse memo instead of borrowed; every Describe recomputed the
RowDescription and every wire-type lookup re-read the type catalog, both now
memoised per connection and invalidated by the catalog version; and a one-row
read decoded the collection's whole options document (through serde's
extended-JSON path) just to ask whether it was a timeseries collection, which
was 40% of the read. An extended-protocol `select 1` is 35us against
PostgreSQL's 30 (was 55), and a row by primary key is 43us against 32 (was 62).

#### Changed

- `crates/secantus-pgserver/src/server.rs`: one OS thread per connection, each
  with its own single-threaded tokio runtime; the accept loop keeps a
  one-worker runtime of its own. `block_in_place` is gone from the statement
  path.
- `crates/vendor/pgwire`: backend messages are `feed`-buffered and flushed once
  per Sync (or ReadyForQuery) rather than written per message; the frame
  reader takes a buffered fast path when a whole message is already in the
  buffer.
- `crates/secantus-pgserver/src/lib.rs`: the parse memo hands out a borrowed
  `Arc<ParseResult>`; a per-connection Describe memo (64 entries, keyed on the
  statement, its parameter types, the result formats and the catalog version —
  FETCH excluded, errors never recorded) and a per-connection wire-type memo
  (bypassed while a transaction holds uncommitted DDL); `Execute` with
  `max_rows = 0` streams the portal without materialising it; parameter-type
  inference short-circuits when every parameter is declared.
- `crates/secantus-storage`: `is_timeseries` reads the raw options blob instead
  of decoding it, and the `_id` point-lookup fast path is gated on the filter
  actually naming `_id`.

#### Added

- `tests/test_rust_pgserver_slice.py`: the Describe and wire-type memos are
  pinned to follow DDL from another connection and DDL inside the same block
  (including rollback), to key on the result format, and to skip FETCH; a raw
  protocol test pins that `Execute` with `max_rows = 0` drains the portal and
  a later Execute on it answers `SELECT 0`. All of them also pass against
  PostgreSQL 16.
- `crates/secantus-storage/tests/timeseries.rs`: a timeseries collection's
  duplicate `_id`s are all visible to a point lookup, count and delete, and a
  plain collection's `_id` lookup is unchanged.
