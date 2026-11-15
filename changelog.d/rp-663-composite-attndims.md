### A composite type's attributes record their array dimensions

`pg_attribute.attndims` now counts the `[]` a composite type's attribute
was written with, as it already did for a table's column: after
`create type c as (p int[][], q int, r text[])` the three attributes read
2, 0 and 1, as on PostgreSQL 15.19, where each used to read 0. `ALTER
TYPE ... ADD ATTRIBUTE` and `ALTER ATTRIBUTE ... TYPE` count the new
type's bounds the same way.

#### Fixed

- A composite type's attribute has the `attndims` it was declared with,
  through `CREATE TYPE` and `ALTER TYPE` (`BuildDescForRelation`).
  Fixes RP-663 (tests/backlog/test_rp_found_rp658.py::test_composite_attribute_attndims)
