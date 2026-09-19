### It was never the transaction block; it was the uncommitted CREATE TABLE

For a while the Rust PostgreSQL server looked as though it charged a standing
tax for working inside a transaction: the same `select 1` cost about 125
microseconds inside an explicit block against 55 outside one, where PostgreSQL
16 is flat at 30 either way. Two rounds of profiling had gone looking for the
hot symbol that owned the difference and had come back with candidates worth a
few microseconds each. The mistake was in the question. Running the statement
both ways and varying one ingredient at a time shows that a bare block is
slightly *cheaper* than autocommit, as it should be — it has no per-statement
transaction to begin and commit. What costs is a block that is holding
uncommitted DDL, and the benchmark had been creating its table inside the block
all along.

Once a block has created a table or a type, the session carries an overlay of
catalog rows nobody has committed yet. Two separate caches back away from that
overlay, and counting calls rather than sampling time showed how far: a
statement in such a block re-read the type catalog from storage twenty times
where the same statement in autocommit read it zero times, and rebuilt the
planner's user-type tables twice per statement where autocommit rebuilt them
never. Both were reacting to the same thing for the same reason — a view that
carries one session's uncommitted DDL must not be published to the others — and
both were using the catalog version number to express it, which cannot tell
"another connection changed the catalog" apart from "I changed it myself". A
block's own first DDL statement bumps that version, so from then on the test
could never pass again and both caches stayed off for the life of the block.

The fix says what was actually meant in each case. The process-wide catalog
cache is filled only from a read that did not run on the transaction's own
WiredTiger session, since that session is the one that can see the block's
uncommitted writes; a read on a fresh session sees exactly the committed
catalog and is publishable wherever it was issued. The planner's per-thread
type tables now record which session owns a view that carries an overlay, so a
session can reuse its own and no other session can pick it up — a worthwhile
distinction, because two connections share worker threads, and dropping the
owner from that key lets one connection cast to a type another connection has
not committed. Inside a block that created a table, `select 1` goes from 153 to
53 microseconds and `select v from t where k = 1` from 134 to 62, with
autocommit unchanged; a statement in a block is now marginally cheaper than the
same statement outside one, which is the shape PostgreSQL has.

#### Fixed

- `crates/secantus-pgserver`: a statement inside a transaction block that had
  done any DDL re-read the whole type catalog twenty times and republished the
  planner's type tables twice, costing ~100us per statement. Both caches now
  distinguish a session's own uncommitted view from the committed one instead
  of switching off for the rest of the block.
- `crates/secantus-pgserver`: the planner's thread-local user-type tables are
  keyed by the session that owns an uncommitted-type overlay, so a view holding
  one connection's uncommitted `CREATE TYPE` can no longer be reused by another
  connection that lands on the same worker thread.

#### Changed

- `bench/pg_statement_cost.py` honours `SECANTUSD_PG`, so a worktree can
  measure its own binary instead of silently measuring the main checkout's, and
  its docstring now says that `--in-transaction` varies "block holding
  uncommitted DDL", not "block".
