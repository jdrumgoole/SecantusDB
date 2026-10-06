### Rust PostgreSQL server: subqueries see READ COMMITTED commits; sub-millisecond timestamps sort and group; simple-protocol SELECTs stream

A batch of fixes and memory work on `secantusd-pg`. Every answer was checked
against PostgreSQL 15.19.

#### Fixed

- **A READ COMMITTED block's uncorrelated subquery missed rows committed
  since the block's snapshot.** A subquery like this runs while the
  statement is planned. Planning came before the block moved onto a fresh
  snapshot. So the first read after another session's commit answered the
  subquery from the old snapshot: `select (select max(x) from t)` gave `2`
  where PostgreSQL gives `300`. A statement with a subquery or a WITH item
  now gets its snapshot before planning.
- **Timestamps that share a millisecond were not told apart in five places.**
  A `timestamp` / `timestamptz` is stored as a millisecond date plus a hidden
  microsecond remainder. Five places compared the date alone:
  - `ORDER BY ts` put `.002006` before `.002000`.
  - `GROUP BY ts` merged every value in one millisecond into one group.
  - `DISTINCT ON (ts)` kept one row per millisecond.
  - `min(ts)` / `max(ts)` lost the microseconds.
  - `count(DISTINCT ts)` undercounted.

  All of these now use the full value, and window peers and ranks use the
  same comparison. Corpus `b62_distinct_ts` gives 0 divergences in 35
  checks, with and without `SECANTUS_PG_GROUP_MEMORY_BYTES=1000`.

#### Changed

- **A READ COMMITTED read with uncorrelated subqueries or plain-SELECT CTEs
  no longer replays the block's write set.** Such a read used to move the
  block onto a new snapshot whenever another session had committed. Now it
  reads in a fresh transaction, with the block's own rows laid over it,
  exactly as a read without a subquery already did. The planning-time
  subquery and the statement share that one transaction. Measured on a debug
  build with 200 / 400 / 800 insert-then-subquery-read pairs, beside a
  session committing to the same table: the replay path takes 4.0 / 15.5 /
  59.8 s and the new path 1.1 / 3.3 / 12.2 s.
- **A lone SELECT sent through the simple query protocol now streams.** This
  covers a SELECT with no WHERE, outside any transaction. Its rows come from
  a reader thread as pgwire sends them, as an extended portal's already did.
  Measured on 300,000 rows of 2 KB after a restart, with the WiredTiger cache
  capped at 64 MB so the result is not hidden behind the cache: peak RSS
  went from 1884 MB to 175 MB.
- **`DISTINCT` over a `timestamp` / `timestamptz` column streams in bounded
  memory.** It used to be materialised. The hidden microsecond remainder is
  now part of the row's identity.
