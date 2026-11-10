### Rust PG server: an array literal whose upper bound passes `int4` is refused

PostgreSQL's `array_in` checks that each dimension's lower bound plus its
length fits an `int4` (`ArrayCheckBounds`) before it reads an element. This
server skipped the check, so `'[2147483647:2147483647]={1}'::int[]` was
accepted and printed back, where PostgreSQL refuses it.

#### Fixed

- An array literal whose dimension decoration puts the upper bound past
  2147483647 is 54000 `array lower bound is too large: <lb>`, raised after
  the decoration is matched against the contents and before any element is
  read, as on PostgreSQL 15. An array parameter sent in binary format
  still lacks the check (RP-795).

Fixes RP-771 (tests/backlog/test_rp_found_rp711.py::test_array_input_refuses_an_upper_bound_past_int4).

