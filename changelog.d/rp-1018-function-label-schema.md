### A PL/pgSQL function outside the search path is named with its schema in CONTEXT

The CONTEXT line of a notice or error raised inside a PL/pgSQL function now
names the function as PostgreSQL's `format_procedure` does: a `pg_temp`
function as `pg_temp_N.f()`, and a function in a schema off the search path
as `schema.f()`. The Rust PostgreSQL server used to print only the bare
name.

#### Fixed

- A function in `pg_temp`, or in a schema not on the search path, is
  labelled with its schema in CONTEXT.
  Fixes RP-1018 (tests/backlog/test_rp_found_rp833.py::test_a_pg_temp_function_is_labelled_with_its_schema)
