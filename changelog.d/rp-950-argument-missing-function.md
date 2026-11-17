### A missing function inside another call's arguments is the error PostgreSQL reports

PostgreSQL analyses a call's arguments before it looks the call itself up,
so `select abs(nosuch1())` fails on `nosuch1()`, at that call. The Rust
PostgreSQL server typed the unknown inner call and then reported the outer
one, `function abs(text) does not exist`. It now answers as PostgreSQL does,
while an error PostgreSQL meets earlier, such as a missing column or
relation, still comes first.

#### Fixed

- A call to a name no function has, inside another call's arguments, is
  42883 for that call at its position.
  Fixes RP-950 (tests/backlog/test_rp_qualified_calls.py::test_enclosing_call_waits_for_its_arguments)
