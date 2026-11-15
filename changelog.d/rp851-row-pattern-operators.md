### Row comparisons take the pattern operators

The `text_pattern_ops` operators `~<~`, `~<=~`, `~>=~` and `~>~` now compare
rows field by field, as PostgreSQL builds a RowCompareExpr of them:
`('a', 'b') ~<~ ('a', 'c')` and `('a', 'b') ~<~ any (select 'a', 'c')` are
true, where the first was 42804 `collations are not supported by type record`
and the second a 0A000 refusal.

#### Fixed

- Rust PG server: a pattern operator between two rows collates each field
  `"C"` rather than the row; a row against a subquery's rows decides each
  pair by `=` and then the pattern operator's strict form.

Fixes RP-851 (tests/backlog/test_rp_found_rp727.py::test_row_comparison_with_a_pattern_operator)
