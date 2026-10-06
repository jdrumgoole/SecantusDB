### The Rust PostgreSQL server skips subquery outputs nothing reads, and streams the joins it used to hold whole

PostgreSQL never computes a FROM-subquery output that nothing reads, so
`select count(*) from (select a/b from t) s` answers even when `b` is zero.
The Rust PostgreSQL server computed every output and raised a division by
zero; it now removes an unread output first, under PostgreSQL's own rules.

Joins that batch 65 still built whole now read in bounded memory: a join with
a subquery, function or LATERAL side, a join inside a transaction block,
GROUPING SETS / ROLLUP / CUBE, and a join whose right side is too big to hold
(that side is now split into partitions on disk). On 300,000 rows of 2 KB with
a 64 MB WiredTiger cache, peak server memory for these queries dropped from
2.9-5.9 GB to 52-290 MB, with byte-identical output.

#### Fixed

- `secantus-pgplan` (`prune_outputs`): a FROM-subquery output (or an output of
  an inlined CTE, or of a UNION ALL arm) that the enclosing query cannot read is
  replaced by NULL before planning, so its errors are not raised. As in
  PostgreSQL, volatile and set-returning outputs are kept, and so are outputs
  under a plain DISTINCT or named by the subquery's own ORDER BY / GROUP BY. A
  subquery that does not plan on its own keeps every output, so parse-analysis
  errors still surface.
- `secantus-pgserver`: a two-table join of a `float8` or `numeric` column to an
  int column (`ON a.f = b.k`) returned no rows on the narrow join path. It now
  compares the values numerically.

#### Changed

- `secantus-pgserver` (`stream_join`, `grace_join`): streamed joins now cover
  subquery, function and LATERAL sides, transaction blocks (read through the
  block's session), and right sides past `SECANTUS_PG_JOIN_INNER_BYTES` (a
  grace hash join that keeps the materialised order). GROUPING SETS are
  aggregated one set at a time in bounded memory. Aggregates over a join no
  longer encode the joined rows only to decode them again.
  `SECANTUS_PG_JOIN_STREAM=0` turns streamed joins off.
