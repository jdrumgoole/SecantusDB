### Rust PostgreSQL server: linear long READ COMMITTED blocks and sorted portals in bounded memory

A READ COMMITTED transaction block that had written and then read another
table while other sessions kept committing there was moved onto a fresh
snapshot before every such read, replaying its whole write set each time --
quadratic in the block's length (800 insert/read pairs took 54 s). A plain read
of a table the block has not written now runs in a fresh read-only
transaction of its own, which is exactly the per-statement snapshot
PostgreSQL takes, and the same 800 pairs take 1.3 s.

An extended-protocol `SELECT ... ORDER BY` outside a block is now streamed:
the reader sorts the rows in runs of at most 16 MB, spills them to anonymous
temporary files and merges them (a LIMIT keeps only the rows it can return),
so a 300,000-row, 600 MB ordered result costs the server what an unordered one
does instead of ~800 MB more.

#### Fixed

- `secantus-pgserver`: a READ COMMITTED block's plain one-table read of a
  table it has not written (nor any catalog) reads in its own transaction
  instead of replaying the block's write set (`rc_reads_apart`,
  `read_apart`).
- `secantus-pgserver`: an IMMUTABLE `LANGUAGE sql` function called over
  constants is run as itself, as PostgreSQL's `evaluate_function` does before
  inlining, so its error carries `SQL function "f" statement 1` (it said
  `during inlining`).

#### Changed

- `secantus-pgserver`: streamed portals accept an ORDER BY of stored columns
  (`external_sort`); per-statement work trimmed -- the DateStyle and TimeZone
  are parsed only when the session's settings change, the catalog collections
  are checked once per connection, the type catalog is read through a
  per-thread copy of the shared cache, and `sql_relations` remembers its
  parse-tree walk per statement text (names are still resolved per call).
