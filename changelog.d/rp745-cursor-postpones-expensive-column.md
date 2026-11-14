### The Rust PostgreSQL server: a sorted cursor postpones an expensive column past its Sort

PostgreSQL plans a cursor's query for the share of its rows it expects to
be fetched, `cursor_tuple_fraction`, which is 0.1 by default. Under a
fraction above 0 the planner leaves an expensive column of the select list
until after the Sort, as it does under a LIMIT. A FETCH then computes it
only for the rows it returns. The Rust PostgreSQL server now plans a
DECLARE CURSOR's query the same way. A cursor over `select k,
to_tsvector(...) ... order by k desc` answers its first row without
computing `to_tsvector` for the rows behind it, as on PostgreSQL 15.19.

Setting `cursor_tuple_fraction` to 1 plans the cursor for its whole result
again, and the column is computed below the Sort for every row.

#### Fixed

- `secantus-pgplan`: `declare_cursor` re-judges the query's sort input
  with `sort_input::cursor_sort_input_columns`, PostgreSQL's
  `tuple_fraction` from `cursor_tuple_fraction` (1 or more is 0, 0 or
  less is 1e-10). `make_sort_input_target` postpones an expensive column
  when that fraction is above 0.
  Fixes RP-745 (tests/backlog/test_rp_found_rp732.py::test_a_sorted_cursor_postpones_an_expensive_column;
  regression test `test_cursor_tuple_fraction_decides_the_postponement`
  in the same file, and the `sort_input` crate test
  `a_cursor_postpones_an_expensive_column_as_a_limit_does`).
