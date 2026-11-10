### Rust PG server: `min` / `max` over a `reg*` or `cidr` value answer the overload's type

PostgreSQL 15 has no `max(regclass)` or `max(cidr)`: the call resolves
through `func_select_candidate` to `max(oid)` or `max(inet)`, and each
input is coerced before it is compared. This server kept the input type, so
`max(x)` over two `regclass` values was described `regclass` and printed
a relation name where PostgreSQL prints the OID.

#### Fixed

- `min` / `max` over any `reg*` type (`regclass`, `regtype`, `regproc`,
  `regrole`, ...) answer an `oid`, each input coerced to its OID first, in
  an aggregate, a window call and `pg_typeof`. Over a `cidr` they answer
  an `inet`.

Fixes RP-706 (tests/backlog/test_rp_found_rp681.py::test_min_max_coerce_their_input_to_the_overloads_type).
