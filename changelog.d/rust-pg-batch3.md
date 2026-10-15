### Rust PostgreSQL server: backlog batch 3

More of the Rust PostgreSQL server's backlog, each fix measured against
PostgreSQL 15.19 and pinned by the backlog test that found it.

#### Fixed

- `pg_typeof` of a column of a domain type names the domain (`rp38_d`),
  where it printed the base type (`integer`); qualified or not, in a select
  list or a WHERE. Part of RP-038
  (`tests/backlog/test_rust_postgres_gaps.py::test_pg_typeof_names_the_domain`).

- Arrays of `ROW(...)` records compare as PostgreSQL's `array_eq` /
  `array_cmp` over `record_eq` / `record_cmp` do: an untyped field reached
  is 42883 (`could not identify an equality operator for type unknown`),
  two field types that differ are 42804 naming the column, and records of
  different widths are 42804 once their common columns are equal. These
  were 0A000, and an all-typed width mismatch answered `false`. Fixes
  RP-025 (`tests/backlog/test_rust_postgres_gaps.py::test_untyped_record_array_compare`).

- An operator's function called by name over columns it does not take
  (`int4pl(text_col, int_col)`) is 42883 `function int4pl(text, integer)
  does not exist`, as on PostgreSQL, where it was worded as the operator
  it is rewritten into. Part of RP-038
  (`tests/backlog/test_rust_postgres_gaps.py::test_batch_residue_fixed[operator-function-typed]`).

#### Added

- Corpora `domain_typeof`, `record_arrays` and `operator_function_calls`.
