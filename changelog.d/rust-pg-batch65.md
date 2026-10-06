### Rust PostgreSQL server: joins and READ COMMITTED reads stream in bounded memory

A join of stored tables no longer builds every joined row before the first one
goes out. The Rust PostgreSQL server reads and hashes each right side first,
within a 64 MB bound, then reads the leftmost table a batch at a time on the
statement's own thread, joining, filtering and handing rows on as it goes; an
aggregate over the join folds them in bounded memory. A 300,000-row join that
held 3.5 GB now holds 25 MB, with byte-identical output. The narrow two-table
path's nested loop over every pair is a hash on the ON equality, so a join it
falls back on is fast too.

A plain SELECT inside a READ COMMITTED transaction block streams through the
block's transaction as well, except when the statement reads apart from the
block in a fresh snapshot of its own. Three missing errors are raised now: a
COLLATE on a type that takes none is caught at planning, a DISTINCT or
DISTINCT ON whose ORDER BY does not match it is 42P10, and rows skipped by
OFFSET still have their select list computed.

#### Changed

- `secantus-pgserver` (`stream_join`): joins of stored tables, and aggregates
  over them, stream outside a transaction block; a right side over
  `SECANTUS_PG_JOIN_INNER_BYTES` (default 64 MB) falls back to the
  materialised path before any row is sent.
- `secantus-pgserver`: the narrow join path matches through a hash on its ON
  equality instead of a nested loop.
- `secantus-pgserver`: a simple-protocol SELECT inside a READ COMMITTED block
  streams unless it runs apart from the block.

#### Fixed

- `secantus-pgplan`: `ORDER BY v COLLATE "C"` over an integer column is 42804
  at planning, over an empty table too.
- `secantus-pgplan`: `SELECT DISTINCT t ... ORDER BY t COLLATE "C"` and a
  DISTINCT ON whose leading ORDER BY items are not its expressions are 42P10.
- `secantus-pgserver`: rows skipped by OFFSET have their select list computed,
  so `exists(select a/b ... offset 5)` raises 22012 as PostgreSQL does.
