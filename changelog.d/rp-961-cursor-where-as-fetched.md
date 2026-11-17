### A cursor's WHERE is tested as each row is fetched

A cursor in the Rust PostgreSQL server now tests a WHERE clause that cannot be pushed down to the storage filter one row at a time, as a FETCH reaches each row. Before, the first FETCH from a cursor over a VALUES list or a set-returning function tested every row, and a cursor over a table tested every row at the DECLARE itself. A row that raises an error, such as `10/k` with `k = 0`, now raises it at the FETCH that reaches the row, as in PostgreSQL, and rows before it come back first. This also holds when another statement in the block makes the cursor read its snapshot early, and when a scrollable cursor moves back to the start.

#### Fixed

- A cursor's per-row WHERE is tested as rows are fetched: through the row stream for a VALUES or function source, and row by row in the streamed table scan (`CursorTail::residual`), which keeps its rows untested when another statement reads the snapshot early.
- Fixes RP-961 (tests/backlog/test_rp_found_rp744.py::test_a_cursors_where_filters_as_it_is_fetched; regression test tests/backlog/test_rp_found_rp744.py::test_a_table_cursors_where_is_tested_as_it_is_read_again)
