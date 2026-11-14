### DROP TYPE of a table's row type is refused for the table, whatever names it

A table's row type belongs to the table: PostgreSQL refuses `DROP TYPE` of
it with `cannot drop type t because table t requires it` and points at DROP
TABLE instead, before it looks at anything that depends on the type. The
Rust PostgreSQL server looked at the routines and casts over the type first,
so a function taking the row type turned the answer into `other objects
depend on it` -- and under CASCADE it dropped that function before refusing.
It now refuses for the table first, under RESTRICT and CASCADE alike, and
leaves every routine and cast in place.

#### Fixed

- `secantus-pg`: DROP TYPE checks a name for a table's row type before
  walking the routines and casts whose signature names it, as
  `findDependentObjects` reads the type's own internal dependency before
  its dependents: `drop type r` with a function over `r`, with or without
  CASCADE and with a cast over it too, is 2BP01 `cannot drop type r because
  table r requires it`, and the function survives, as on PostgreSQL 15.19.
  Fixes RP-562 (tests/backlog/test_rp_found_rp546.py::test_drop_type_of_a_row_type_with_a_routine)
