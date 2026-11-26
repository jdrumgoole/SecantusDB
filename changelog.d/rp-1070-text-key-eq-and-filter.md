### Rust PG server: an indexed `=` on a text key ANDed with a filter is an Index Scan in EXPLAIN

`explain select b from tp where a = 'x1' and b < 0` over an analysed
table with a `text` PRIMARY KEY (or UNIQUE) printed a Seq Scan with both
clauses as its Filter; PostgreSQL prints `Index Scan using tp_pkey`, the
`=` its `Index Cond` and `b < 0` its `Filter`, at `cost=0.28..8.30`.

#### Fixed

- The scan cost model gave up on any index whose leading column is text,
  because ANALYZE's correlation for text is not trusted, and EXPLAIN fell
  back to the storage's plan, which reads no index for an `AND`. It now
  prices the index scan wherever `cost_index`'s heap I/O does not depend
  on the correlation (its two bounds equal, as when one row is fetched),
  and still declines where it does. Plans and costs now match PostgreSQL
  15 for a unique `=` on a text key with or without another filter.
  A non-unique text index, a `char(n)` key and an expression index still
  differ (RP-1070, trimmed).

Fixes RP-1070 in part (tests/backlog/test_rp_found_rp693.py::test_explain_indexed_equality_with_a_filter; crates/secantus-pgplan/src/costsize.rs::unknown_correlation_tests).
