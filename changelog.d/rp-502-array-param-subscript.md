### A SQL function's array parameter can be subscripted

`create function h(v int[]) returns int language sql as $$ select v[1] $$`
now answers `4` for `h(array[4,5])`, as on PostgreSQL 15.19. The body's
reference to a parameter by name is bound as `$1::int4[]`, and the `[1]`
after it was read as part of that cast's type name, so the call answered
22P02 `invalid input syntax for type integer: "{4,5}"`, and `v[1] + 1` 0A000.

#### Fixed

- A parameter followed by a subscript is bound as a parenthesised cast,
  `($1::int4[])[1]`, so the subscript reads the array.

Fixes RP-502 (tests/backlog/test_rp_function_bodies.py::test_array_parameter_subscript)
