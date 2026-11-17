### A partitioned table's indexes have no size

A partitioned table and a partitioned index have no storage of their own, so
PostgreSQL sizes them at 0: `pg_relation_size` of a partitioned index, and
`pg_indexes_size` and `pg_total_relation_size` of a partitioned table, rows in
its partitions or none. The size functions answered a page per index instead,
sized from a B-tree model that holds the whole tree's rows. They now answer 0,
as PostgreSQL does; each partition keeps its own size.

#### Fixed

- Rust PG server: the size functions answer 0 for a partitioned table and
  its indexes.
  Fixes RP-949 (tests/backlog/test_rp_found_rp931.py::test_a_partitioned_tables_indexes_have_no_size)
