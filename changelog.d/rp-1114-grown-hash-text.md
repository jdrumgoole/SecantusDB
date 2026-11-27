### Rust PG server: a chain of three tables whose hash table grows returns its rows in PostgreSQL's order

When a hash join's table holds more rows than its estimate sized it for,
PostgreSQL doubles its bucket count as it builds the table. At the end of
the build it rebuilds the bucket chains, walking its 32 kB memory chunks.
The rows of one key then come out rotated at a chunk boundary instead of
simply reversed. The two-table join already modelled this, but only for
tuples of one fixed-length type. A chain of three tables did not model it
at all. Now both size each hashed tuple as PostgreSQL lays it out
(`heap_compute_data_size`): a text value of up to 126 bytes packed behind
a one-byte header, and each fixed-length column at its alignment. The
three-table chain applies this to the relation each of its hash joins
hashes. That relation's columns are its target list, in the order
`build_joinrel_tlist` builds it: its tables in range-table order, and in
each table the columns the query reads, in the order its text reads them,
then its join keys.

#### Fixed

- `select x.c, y.c, z.c from t5 z left join (u1 x join t4 y on x.b = y.b)
  on z.b = x.a` (never analysed: 7, 300 and 1,000 rows) returns its 858
  rows in PostgreSQL 15.20's order. The 143 rows of `x.c = 'u6'` start
  `x85, x78, ...`, as there, where they started `x995, x988, ...`. The
  pair's Hash grows from 8,192 buckets to 65,536.
- A sweep of 168 three-table chains with text columns (7 shapes, 6
  select lists, 4 sets of table sizes) now matches PostgreSQL on 67, against
  57 before. None that matched before was lost, and the rows were the same
  in every query. The 375 integer chains of `pg_join_order_matrix.py
  --filter multi3` match before and after.
- Filed RP-1169: a hash table that outgrows `work_mem` while it is built
  splits into batches (`Batches: 4 (originally 1)`) and returns some keys'
  rows in another order.
- Filed RP-1170: over larger tables, `u1 x join t4 y ... left join t5 z
  ...` is joined in another order than PostgreSQL's Merge Join of `y`
  with the Hash Right Join of `z` and `x`.

Fixes RP-1114 (tests/backlog/test_rp_hashed_pair_order.py::test_a_hashed_merge_pair_returns_a_keys_rows_in_postgres_order)
