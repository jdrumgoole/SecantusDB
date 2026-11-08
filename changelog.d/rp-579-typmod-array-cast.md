### Rust PG server: a cast to an array with a type modifier applies it to each element

`select '{abcd}'::char(3)[]` answered `{abcd}`, where PostgreSQL 15.19
answers `{abc}`; `'{ab}'::char(3)[]` answered `{ab}` for `{"ab "}`, and
`'{1.234}'::numeric(5,2)[]` `{1.234}` for `{1.23}`. The cast read the
array and dropped the modifier written on its element type.

#### Fixed

- A cast to `char(n)[]`, `varchar(n)[]`, `numeric(p,s)[]`,
  `timestamp(p)[]`, `bit(n)[]` or `interval ...[]` reads the literal as the
  array at typmod -1, as `array_in` does, and then runs the element type's
  explicit length coercion over every element, as `coerce_type_typmod`'s
  `ArrayCoerceExpr` does: `char(n)` and `varchar(n)` truncate, `char(n)`
  pads, `numeric(p,s)` rounds and refuses what does not fit with 22003
  (`'{123456}'::numeric(5,2)[]`). A NULL element stays NULL, and the
  array's bounds and nesting are kept. The same holds for a cast of an
  array value (`array['abcd']::char(3)[]`) and per row.

Fixes RP-579 (tests/backlog/test_rp_found_rp570.py::test_cast_to_typmod_array),
with the crate test `tests::typmod_array_cast_coerces_each_element`.
