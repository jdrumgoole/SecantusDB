### Rust PG server: a scalar compared with an array value names the SQL types

A scalar compared with an array that only appears as the query runs -- a
scalar subquery returning an array, a VALUES column, `= any (select
array[...])` -- is 42883 `operator does not exist: integer = integer[]`, as
on PostgreSQL 15.19, instead of naming the internal `int4 = int4[]`.

#### Fixed

- The per-row scalar / array mismatch, and the same check against a table
  column in a pushed-down WHERE, print the types through `display_type`
  (`integer`, `bigint`, `numeric`, `boolean`, `character varying`).

Fixes RP-742 (tests/backlog/test_rp_found_rp722.py::test_a_scalar_against_an_array_value_names_its_types)
