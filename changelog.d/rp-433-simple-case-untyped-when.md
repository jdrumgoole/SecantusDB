### Rust PG server: a simple CASE reads a quoted WHEN as its argument's type

A simple CASE (`case a when '1' then ...`) compares its argument to each WHEN
with `=`. PostgreSQL builds that comparison during parse analysis, so a quoted
WHEN has type UNKNOWN and takes the argument's type. Here the WHEN stayed a
string, and any argument that was not text, such as an integer, a numeric or a
boolean, was refused with 0A000 `comparing int32 with string using = is not
supported yet`. That included the common `sum(case a when '1' then 1 else 0
end)`.

#### Fixed

- `secantusd-pg`: in a simple CASE, a quoted WHEN is read by the argument
  type's input function, as on PostgreSQL 15.19: `case 1 when '1'`,
  `case 1.5 when '1.5'` and `case interval '1 day' when '24 hours'` take
  their branch. A WHEN that type's input refuses is its 22P02 at the
  literal, raised when the statement is analysed even if no row reaches it
  (`case a when 't'` over an integer `a` is `invalid input syntax for type
  integer: "t"`).

Fixes RP-433 (tests/backlog/test_rp_where_types.py::test_simple_case_types_untyped_when_from_its_argument).

#### Filed

- RP-439: a simple CASE whose WHEN is typed in a different category from its
  argument (`case 1 when true`) is still 0A000 here. PostgreSQL answers 42883
  `operator does not exist: integer = boolean` at the WHEN.
