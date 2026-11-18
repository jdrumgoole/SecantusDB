### A row pattern comparison over numbers is refused as PostgreSQL refuses it

`select (1, 2) ~<~ (1, 3)` is now 42883 `operator does not exist: integer
~<~ integer` at the operator, as on PostgreSQL, which looks up each field
pair's operator and has no `~<~` for numbers. It was 42804 `collations are
not supported by type integer` with no position: the server collated each
field `"C"` before anything checked the pair.

#### Fixed

- `pattern_ops` refuses a row pair typed on its face (a constant or a
  cast) with no string side before it rewrites the operator. Two columns'
  pair stays open as RP-973.
- Fixes RP-973 (tests/backlog/test_rp_found_rp727.py::test_row_pattern_operator_over_non_strings)
