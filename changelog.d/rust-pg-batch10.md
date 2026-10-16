### Rust PostgreSQL server: backlog batch 10

More of the Rust PostgreSQL server's backlog, measured against PostgreSQL
15.19.

#### Fixed

- **A GROUP BY or DISTINCT that PostgreSQL sorts comes out sorted.** The
  planner prices a HashAggregate against a Sort + Group over the cheapest
  way to read the table, and keeps the sorted plan when the two cost the
  same within 1%, as they do over a few estimated rows. The server now
  makes that choice from PostgreSQL's own cost model
  (`secantus-pgplan/src/costsize.rs`): sequential, bitmap heap and index
  scans for a table with no statistics, sized from the heap pages its rows
  really take; the sort, group and aggregate costs; and `add_path`'s fuzzy
  comparison. An aggregate with DISTINCT or its own ORDER BY, which
  PostgreSQL never hashes, comes out sorted too. Part of RP-028
  (tests/backlog/test_rust_postgres_gaps.py::test_few_estimated_rows_group_by_sorting).
- **A GROUP BY or DISTINCT under a LIMIT reads an index when PostgreSQL
  does**: the planner takes the plan cheapest to the rows the LIMIT keeps,
  which over a whole table is a Group over an index in the keys' order, so
  the first keys come back in order. Part of RP-028
  (tests/backlog/test_rust_postgres_gaps.py::test_limit_reads_the_index_in_key_order).
- **A GROUP BY holding a table's whole primary key groups by the key
  alone**, as PostgreSQL does, so its HashAggregate order is the key's.
  Part of RP-028
  (tests/backlog/test_rust_postgres_gaps.py::test_group_by_a_whole_primary_key_hashes_on_it).
- **An ORDER BY over group keys moves them to the front of the GROUP BY**,
  which changes the HashAggregate's order and so where an ORDER BY leaves
  ties. Part of RP-028
  (tests/backlog/test_rust_postgres_gaps.py::test_order_by_moves_its_group_keys_first).
- **A `SELECT DISTINCT` read through the extended protocol comes out in the
  HashAggregate's order.** Batch 9 gave DISTINCT that order, but a portal
  streamed the rows through a sort instead, so psycopg saw them sorted; a
  DISTINCT with no ORDER BY now streams only past 200,000 rows. Part of
  RP-028
  (tests/backlog/test_rust_postgres_gaps.py::test_distinct_rows_come_out_in_hash_aggregate_order).
