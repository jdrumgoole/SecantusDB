### A row pattern comparison over columns that are not strings is refused

A row comparison with a text pattern operator, such as `(a, b) ~<~ (1, 2)`
over integer columns, is now refused as PostgreSQL refuses it: each pair
of fields is looked up as that operator, which only string types have, so
the statement is 42883 `operator does not exist: integer ~<~ integer` at
the operator. Before, a pair typed only by its columns was compared as
the rows, or failed while running with a collation error.

#### Fixed

- `pattern_ops` records the row comparisons it rewrites, and the planner's
  typing pass checks each pair once its columns are typed, an untyped
  literal or NULL counting as `unknown` (`integer ~<~ unknown`).
- Fixes RP-973 (tests/backlog/test_rp_found_rp727.py::test_row_pattern_operator_over_non_string_columns)
