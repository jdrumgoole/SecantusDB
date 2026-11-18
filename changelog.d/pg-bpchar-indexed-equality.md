### An equality on a bpchar key is an index scan that ignores trailing blanks

A `WHERE a = 'k'` over an unsized `bpchar` PRIMARY KEY or UNIQUE column now
reads the key the server keeps with its trailing blanks trimmed, the way
PostgreSQL's btree over `bpcharcmp` does: the comparison is pushed down to
storage and EXPLAIN shows an Index Scan on the key, where it had scanned the
table and compared every row. `'k'`, `'k  '::bpchar` and a stored `'k  '`
are the same key; a `text` constant still compares with `a::text`.

#### Fixed

- Rust PG server: an `=`, `IN` or range comparison of an unsized `bpchar`
  PRIMARY KEY or UNIQUE column against a constant reads its trimmed key,
  `bpchareq` pushed down, and EXPLAIN shows `Index Scan using <key>` as
  PostgreSQL 15 does (RP-693 step 4; RP-1070 filed).
  (tests/test_rust_pg_bpchar_key.py::test_an_indexed_equality_reads_the_trimmed_key)
