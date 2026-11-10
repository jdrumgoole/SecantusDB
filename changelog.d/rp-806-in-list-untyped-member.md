### Rust PG server: an IN list's untyped member is read as the list's type when the statement is analysed

`select 1 in (1, 'x')` answered `true`, where PostgreSQL 15.19 refuses the
statement with 22P02 `invalid input syntax for type integer: "x"`. The
members were compared one at a time, so the first match ended the list
before `'x'` was ever read, and with no rows nothing was checked.

#### Fixed

- An IN list's untyped string members are now read as a type while the
  statement is analysed, as PostgreSQL's `transformAExprIn` does. When more
  than one member reads no column, that type is the common type of the left
  operand and those members (`1 in (1.5, 'x')` is the `numeric` input
  error). Otherwise each member is a plain `=`, and the literal takes the
  left operand's type. A member the type's input refuses is its 22P02 or
  22003 at the literal's position. This holds for a select list, a WHERE,
  NOT IN, and a statement over no rows (`... where false`, an empty table).
  Fixes RP-806 (tests/backlog/test_rp_found_rp792.py::test_an_in_lists_unreadable_member_is_an_error)
  and `crates/secantus-pgplan/src/tests.rs::an_in_lists_untyped_member_is_read_as_the_lists_type_at_planning`
  pins the types and positions.
