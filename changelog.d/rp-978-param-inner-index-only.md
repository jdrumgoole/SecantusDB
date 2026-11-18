### A parameterized join's inner side reads its key index-only

`EXPLAIN` of an inner join narrowed to a few rows, whose inner side reads
nothing but its unique key, now prints PostgreSQL 15.19's nested loop into
an `Index Only Scan` of that key's index, priced as PostgreSQL prices it,
including the heap fetches a vacuumed table's visibility map saves. A
select list naming only some columns no longer leaves such a join with
zero estimates when the WHERE bounds the join key.

#### Fixed

- Rust PG server: a parameterized nested loop's inner side that reads only
  its key is an `Index Only Scan`, costed with the all-visible fraction;
  the narrow two-table join path prices a WHERE bounding a join key.
  Fixes RP-978 in part (tests/backlog/test_rp_explain_costs.py::test_plan[a parameterized nested loop's inner read index-only],
  tests/backlog/test_rp_explain_costs.py::test_vacuumed_parameterized_inner_reads_index_only).
