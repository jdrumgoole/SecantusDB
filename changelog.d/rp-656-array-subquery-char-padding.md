### Rust PG server: an `ARRAY(select ...)` over a `char(n)` column keeps its blanks

An ARRAY subquery is now the array of its column's type at the column's
modifier, as `exprTypmod` makes it on PostgreSQL. A `char(n)` value is
stored without its trailing blanks, and the subquery's values were cast to
`bpchar[]`, which has no width to pad them back to. They are now cast to
`character(n)[]` (or `character varying(n)[]`), so the blanks come back and
the RowDescription carries the column's modifier.

#### Fixed

- `select array(select c from t)` over a `char(3)` column holding `'xy'`
  is `{"xy "}`, through a WITH and a UNION ALL too, and describes modifier
  7, as on PostgreSQL 15.19. A `char(3)[]` column's elements keep their
  blanks the same way, and `array(select c from t) = array['xy
  ']::char(3)[]` is true.

Fixes RP-656 (tests/backlog/test_rp_found_rp569.py::test_array_subquery_keeps_char_padding, tests/backlog/test_rp_found_rp656.py::test_array_sublink_is_typed_at_its_columns_modifier)
