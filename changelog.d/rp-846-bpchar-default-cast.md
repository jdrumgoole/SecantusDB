### A `char(n)` column's DEFAULT is labelled `::bpchar`

The Rust PostgreSQL server now labels a literal column DEFAULT as
PostgreSQL's ruleutils does, by `format_type_with_typemod` of the
constant's type with no modifier. `information_schema.columns.column_default`
and `pg_get_expr(adbin, adrelid)` of `char(4) default 'ab'` are
`'ab'::bpchar`, as on PostgreSQL 15.19, instead of `'ab'::character`, which
reads back as `character(1)`. The same rule makes a `char(n)[]` default
`'{a}'::bpchar[]` and a `bit(n)` default `'101'::"bit"`.

#### Fixed

- A `char(n)`, `char(n)[]` or `bit(n)` column's literal DEFAULT is printed
  with the cast PostgreSQL prints, `::bpchar`, `::bpchar[]` or `::"bit"`.
  Fixes RP-846 (tests/backlog/test_rp_found_rp839.py::test_a_char_columns_default_casts_to_bpchar),
  pinned too by tests/test_rust_pgserver_slice.py::test_a_literal_defaults_cast_names_its_type_without_a_modifier.

#### Filed

- RP-863: a DEFAULT written as an explicit cast with a modifier
  (`'ab'::char(4)`) drops the modifier.
- RP-864: a `timestamp`, `timestamptz`, `interval` or `bytea` DEFAULT
  prints the stored value's internal form instead of a literal.
