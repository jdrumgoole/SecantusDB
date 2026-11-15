### A PL/pgSQL reference that names a variable and a column is ambiguous

A query inside a PL/pgSQL body that names both a variable and a column of one
of its FROM items -- `r.a` where `r` is a record variable and a table aliased
`r` has a column `a`, or a bare `v` beside a column `v` -- is now refused with
42702 `column reference "..." is ambiguous`, as PostgreSQL's default
`plpgsql.variable_conflict = error` has it, with its detail line and the
error pointing into the query. Before, the variable silently won, or the
substitution left a syntax error.

#### Fixed

- Rust PG server: PL/pgSQL raises 42702 for a column reference that names
  both a variable and a FROM item's column, with PostgreSQL's DETAIL,
  internal query and position; an error the binder raises with a position
  in a statement is reported as an internal query, as `_SPI_error_callback`
  does. Fixes RP-838 (tests/backlog/test_rp_found_rp689.py::test_a_column_a_record_field_shares_is_ambiguous)
