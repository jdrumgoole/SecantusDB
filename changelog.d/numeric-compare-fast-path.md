### Numeric comparisons stop allocating on the hot path

Every numeric comparison in the Rust engines — a find filter's
`$gt`/`$eq`/range test, a sort comparator call, an `$expr` compare — used to
build the value's exact decimal-digit form on the heap (a `String` plus a
digit vector per operand) before comparing. A new allocation-free fast path
answers the common int32/int64/double pairs directly, falling back to the
digit form only for Decimal128 and for int64↔double pairs beyond ±2^53
(where the engines' shortest-repr decimal semantics and exact binary
comparison can diverge — the boundary is proven and pinned by an
edge-corpus equivalence test). Measured on COLLSCAN drains: +11% on an
integer range filter, +49% when an integer query bound meets a double
field; all seven Rust↔Python parity suites unchanged.

#### Changed
- `secantus-core`: `numeric::fast_cmp` / `fast_eq` / `fast_cmp_numberish`
  answer int/double comparisons without allocating; the query matcher,
  `order::cmp` / `bson_lt`, and the expression engine's compare/eq paths
  try them first. Decimal128 and out-of-range pairs keep the exact
  digit-form path; verdicts are byte-for-byte unchanged.
