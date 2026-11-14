### A FOR loop or an OPEN over `EXECUTE ... USING` binds its values

`for x in execute 'select $1' using 5 loop` and `open c for execute '...'
using 7` used to run the dynamic query with no parameters, so the first
`$1` was 42P02 `there is no parameter $1`. The Rust PostgreSQL server now
evaluates the USING values as PostgreSQL does (`exec_eval_using_params`)
and binds them to the portal, as it already did for `EXECUTE ... USING`.

#### Fixed

- A PL/pgSQL `FOR ... IN EXECUTE ... USING` loop and an `OPEN ... FOR
  EXECUTE ... USING` bind their USING values.
- Fixes RP-906 (tests/backlog/test_rp_found_rp828.py::test_a_dynamic_portal_binds_its_using_values)
