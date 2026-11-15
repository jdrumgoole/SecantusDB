### An explicit cast in a DEFAULT keeps its modifier in the catalog

A column default written as `'ab'::char(4)` or `'ab'::varchar(5)` now reads
back from `information_schema.columns` and `pg_get_expr` as
`'ab'::character(4)` / `'ab'::character varying(5)`, as PostgreSQL prints the
explicit length coercion, instead of the bare `'ab'::bpchar`.

#### Fixed

- An explicit-cast DEFAULT to a sized `char(n)` / `varchar(n)` lost its
  modifier in `column_default`. Fixes RP-863 (tests/backlog/test_rp_found_rp846.py::test_an_explicit_cast_default_keeps_its_modifier)
