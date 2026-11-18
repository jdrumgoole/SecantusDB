### View definitions keep a name column's type in a mixed string IN list

`pg_get_viewdef` resolved any mix of string types to `text`, so a view filtering `relname in ('g'::text, 'h')` printed the column cast to text instead of the list cast to `name`. The printer now resolves string types the way PostgreSQL's `select_common_type` does: the first type stays unless it coerces implicitly to the next one and not back. `text` and `name` keep each other's order, and a `varchar` or `bpchar` meets `name` at `name`. The same rule types UNION columns and JOIN USING columns in printed view definitions.

#### Fixed

- Rust PG server: `pg_get_viewdef` resolves a mix of string types with `select_common_type`'s rule rather than always taking `text`.
  Fixes RP-1045 (tests/backlog/test_rp_found_rp782.py::test_pg_get_viewdef_of_a_name_in_list_with_a_text_element)
