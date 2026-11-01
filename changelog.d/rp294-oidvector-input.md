### Rust PG server: `int2vector` / `oidvector` values read by their input functions

A built-in function's `pg_proc.proargtypes` is now the vector of type oids
PostgreSQL holds, so `proargtypes::regtype[]` and `proargtypes::oid[]`
answer as they do on PostgreSQL 15.19, and comparing a vector with a quoted
literal (`indkey = '1'`, `proargtypes = '16 16'`) reads the literal as a vector.

#### Fixed

- A COPY'd `int2vector` / `oidvector` field, which is how the built-in
  catalog rows are loaded, went through the array decoder: a one-element
  vector became an integer and a longer one a string, so
  `proargtypes::regtype[]` was refused with "cannot cast int4 to regtype[]".
  It is now parsed as `int2vectorin` / `oidvectorin` parse it.
- A quoted literal compared with a vector column, in WHERE or in the
  select list, is read as that vector type instead of matching nothing or
  failing as a malformed array literal.

Fixes RP-294 (tests/backlog/test_rp_catalog_reads.py::test_found[an oidvector casts to regtype[]])
