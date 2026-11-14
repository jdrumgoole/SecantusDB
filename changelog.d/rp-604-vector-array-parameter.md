### Rust PG server: a parameter declared `int2vector[]` or `oidvector[]` has its type

A client that binds `{"1 2",3}` as type oid 1006 is passing an
`int2vector[]`. On PostgreSQL 15.19 `pg_typeof($1)` is `int2vector[]`,
`($1)[2]::text` is `3` and `cardinality($1)` is 2, in text or binary
format. Here the server had no name for oids 1006 and 1013, so the value
stayed the client's raw text. `pg_typeof($1)` was 42P18 `could not
determine data type of parameter $1`, and the other two failed with
42804 / 42883. A single `int2vector` or `oidvector` sent in binary was
refused too.

#### Fixed

- Oids 1006 and 1013 are `int2vector[]` and `oidvector[]` to the planner.
  A text value is read by the array type's input, and each element by
  `int2vectorin` / `oidvectorin`. A binary value is read by `array_recv`,
  each element by the vector's receive function. Every element is kept
  as one vector.
- A binary `int2vector` / `oidvector` (oid 22 / 30) is decoded, alone or as
  an array element. Its shape is held to `int2vectorrecv` /
  `oidvectorrecv`'s rules: one dimension, a lower bound of 0, no NULL and
  not empty. Anything else is 22P03 `invalid int2vector data` (or
  `oidvector`), at Bind, as on PostgreSQL.

Fixes RP-604 (tests/backlog/test_rp_found_rp575.py::test_vector_array_parameter).
The crate regression test is `secantus-pg`'s
`binary_recv::tests::a_vector_is_received_as_postgresql_receives_one`.
