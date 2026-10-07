### Rust PostgreSQL server: joins past the memory bound skip the spill when the other side fits, and GROUPING() works over joins and in HAVING

A join whose right side is too big to hold in memory used to write that whole
side to partition files before it had looked at the left side. The server now
reads the left side first. If the left side fits, it keeps it in memory and
reads the right table again in chunks, so nothing goes to disk. Only when
neither side fits does it partition, as before. Grouping sets over wide
aggregate inputs (a `max()` over a long text column, say) used to read the
input once per set. Now they read it once: each set's groups are hashed and
their partial results combined, so the memory used grows with the number of
groups, not with the rows.

Running queries against PostgreSQL 15.19 turned up three divergences in
`GROUPING()` and ORDER BY, all now fixed. `GROUPING()` was refused with a
grouping error over any join. It was also refused in a HAVING clause. And an
output alias inside an ORDER BY expression was taken for the output column
where PostgreSQL resolves it to an input column.

#### Changed

- `secantus-pgserver` `grace_join.rs` / `stream_join.rs`: a right side past
  `SECANTUS_PG_JOIN_INNER_BYTES` triggers a read of the left side; a left
  side within the bound is held and joined against the right table a chunk at
  a time (`left_built_join`), in the materialised path's row order. Otherwise
  the left side is spooled and the right side partitioned (`grace_join`).
  Both the narrow two-table path and the general join planner use it.
- `secantus-pgserver`: grouping sets whose slim rows are as wide as the rows
  are grouped from one read when every aggregate's partials combine exactly
  (`count`, `min` / `max`, `bool_and` / `bool_or`, integer / numeric `sum`).
  When more groups than the bound holds turn up, it falls back to one read
  per set.
- `secantus-pgplan`: `infer_param_types` remembers its answer per statement
  text and declared types (per thread). libpq declares no parameter types,
  so a prepared statement was parsed again at every Execute (~0.9 us of a
  primary-key read).

#### Fixed

- `secantus-pgplan`: `GROUPING(...)` over a join (in the select list, ORDER BY
  or HAVING) was 42803 "arguments to GROUPING must be grouping expressions";
  the expression walker now reaches its arguments.
- `secantus-pgplan`: `GROUPING(...)` in HAVING was 0A000; it is computed for
  the test like a hidden aggregate. A misplaced `GROUPING(k)` in HAVING is
  PostgreSQL's 42803 with its position.
- `secantus-pgplan`: over a join, an ORDER BY expression (`-g`,
  `name || 'x'`, `grouping(name)`) now resolves names to input columns. Only
  a bare ORDER BY key may name an output column, as in PostgreSQL. An output
  alias used inside an expression is 42703 (it was a confusing 42803 naming a
  hidden column).
- Corpus `b68_wide_sets` (36 lines, 0 against PostgreSQL 15.19, also with the
  join and group bounds at 20,000 bytes and at 1 byte, and with streaming
  off); slice test `test_batch68_left_built_joins_and_hashed_grouping_sets`.
