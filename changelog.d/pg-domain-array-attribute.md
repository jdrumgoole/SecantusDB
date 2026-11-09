### Rust PG server: a column of a domain's array type is in `pg_attribute`

A table column declared with a domain's array type (`x d[]`) was missing
from `pg_attribute`, so `\d` and every client that reads the catalog saw
the table without it, although `select *` and `information_schema.columns`
saw it.

#### Fixed

- The lookup from a type's name to its oid did not know a domain's array
  type, although `pg_type` has listed it since RP-559, so the column's row
  was skipped. A column declared `d[]` or `s.d[]` is now listed with the
  domain's `typarray`, as on PostgreSQL 15.19.
- The same lookup refused a composite's `ALTER TYPE ... ADD ATTRIBUTE b
  d[]` with 42704 `type does not exist`. It now adds the attribute, as
  PostgreSQL does.
- Fixes RP-653 (tests/backlog/test_rp_found_rp607.py::test_domain_array_column_in_pg_attribute,
  tests/backlog/test_rp_found_rp607.py::test_domain_array_in_a_schema_and_a_composite).

#### Found

- RP-658: a user column's `attndims` is always 0, where PostgreSQL gives
  the number of dimensions the column was declared with
  (tests/backlog/test_rp_found_rp653.py::test_array_column_attndims).
