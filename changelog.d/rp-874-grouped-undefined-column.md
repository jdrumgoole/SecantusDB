### A grouped query names a missing column as missing

A select-list column that no FROM item has, in a query with GROUP BY, is
now 42703 `column "nosuch" does not exist`, as PostgreSQL reports it while
the select list is analysed. It used to be reported as an ungrouped column
(42803).

#### Fixed

- Rust PG server: `select nosuch from t group by a` is 42703, not 42803.
  Fixes RP-874 (tests/backlog/test_rp_found_rp854.py::test_an_undefined_column_in_a_grouped_query_is_42703)
