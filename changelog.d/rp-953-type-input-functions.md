### A type's input function can be called by name

`jsonb_in('{"b":1, "a":2}')`, `int4in('12')` and the other base types' input
functions now run, as on PostgreSQL, where an untyped literal coerces to
their `cstring` argument. They were refused as not supported.

#### Fixed

- Every base type's input function in PostgreSQL 15.19's `pg_type`
  (`pg_type_input.tsv`) is the I/O conversion to that type. A `text`
  argument is refused 42883 as PostgreSQL refuses it, and an error in the
  input has no position, since the call runs at execution. The
  three-argument forms (`numeric_in`, `varcharin`, ...) run for typmod -1;
  a typmod stays open as RP-953.
- Filed RP-1041: `'x'::cstring::int4` is accepted, where PostgreSQL has no
  such cast.
- Fixes RP-953 (tests/backlog/test_rp_found_rp750.py::test_type_input_function_is_callable_by_name)
