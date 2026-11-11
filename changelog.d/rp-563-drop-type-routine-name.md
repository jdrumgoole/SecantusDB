### Rust PG server: DROP TYPE names a routine as PostgreSQL does

PostgreSQL writes a routine two ways. A DETAIL line, a CASCADE notice or a
2BP01 headline uses `getObjectDescription`, which is `format_procedure`'s
`function k(integer,ty)` with a bare comma. A lookup error (42883 `does not
exist`, 42809 `is not a procedure`) uses `func_signature_string`, with a
comma and a blank. DROP TYPE and DROP FUNCTION used the lookup form for both.

#### Fixed

- `drop type ty563` with `k563(int, ty563)` over it now says `function
  k563(integer,ty563) depends on type ty563`, as PostgreSQL 15.19 does.
  The same change covers the CASCADE notice, a base type's I/O functions,
  and the casts, I/O types and triggers that refuse a DROP FUNCTION.
  42809 and 42883 keep the blank, as on 15.19.
- Fixes RP-563 (tests/backlog/test_rp_found_rp546.py::test_drop_type_names_a_routine_as_format_procedure);
  crate test `row_type_depends::tests::format_procedure_joins_by_a_bare_comma`.
