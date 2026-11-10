### Rust PG server: a binary array parameter is held to `array_recv`'s size and bound checks

PostgreSQL's `array_recv` counts a binary array's elements
(`ArrayGetNItems`) and checks each dimension's upper bound against `int4`
(`ArrayCheckBounds`) before it reads an element. This server checked
neither at Bind. An `int4[]` parameter with lower bound 2147483647 bound
and printed back. A negative dimension bound as an empty array. A
dimension past `MaxArraySize` was refused as 08P01 for its missing
elements, not as the size error.

#### Fixed

- An array parameter in binary format whose upper bound passes 2147483647
  is 54000 `array lower bound is too large: <lb>`, as on PostgreSQL 15,
  which already held for a text literal (RP-771).
- One with a negative dimension, a running element count that leaves
  `int4`, or more than 134217727 elements is 54000 `array size exceeds the
  maximum allowed (134217727)`. The overflow is checked at each dimension
  and the limit only on the total, as in PostgreSQL: `[200000000, 0]` is an
  empty array, `[65536, 65536, 0]` is refused.

Fixes RP-795 (tests/backlog/test_rp_found_rp771.py::test_binary_array_refuses_an_upper_bound_past_int4).
