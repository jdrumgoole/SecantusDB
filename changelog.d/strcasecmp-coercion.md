### `$strcasecmp` coerces its operands like mongod

`$strcasecmp` previously required both operands to be strings and raised a
generic `TypeMismatch` (14) otherwise. Real mongod `$toString`-coerces each
operand first — a number becomes its string form, `null` becomes the empty
string — and rejects only a boolean. SecantusDB now does the same, matching
mongod 7.0.12.

#### Fixed

- `$strcasecmp` now coerces a non-string operand to a string (`null` → `""`,
  numbers → their string form) instead of raising, so `{$strcasecmp: [5, "a"]}`
  returns `-1` like mongod. A boolean operand raises `Location16007` (mongod's
  code). Integer coercion computes on both the Python and Rust servers; the
  Rust core defers double/date coercion to Python.
