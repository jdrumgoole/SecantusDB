### A row against a subquery checks each pair's operator up front

A row compared with a subquery's row now resolves the operator for each pair
of fields as the statement is analysed, as PostgreSQL's
`make_row_comparison_op` does: `(a, c) = (select 1, 2)` with `c` text is 42883
`operator does not exist: text = integer` at the operator, and so is a
correlated `IN`, even when the subquery returns no rows.

#### Fixed

- Rust PG server: a row-against-subquery pair with no operator was a
  position-less 0A000 at run time, and no error at all over no rows.

Fixes RP-728 (tests/backlog/test_rp_found_rp697.py::test_row_pair_with_no_operator_is_42883)
