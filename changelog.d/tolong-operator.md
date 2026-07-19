### `$toLong` aggregation operator

The `$toLong` conversion operator is now implemented, completing the `$to*`
conversion family (`$toInt` / `$toDouble` / `$toDecimal` / `$toBool` /
`$toString` / `$toDate` were already present, and `$convert: {to: "long"}` too).
It converts numbers (truncating a double toward zero), numeric strings, and
booleans to a 64-bit `long`, matching real mongod 7.0.12 — so a value beyond the
32-bit range that `$toInt` rejects converts cleanly, while a value beyond the
64-bit range overflows (code 241, catchable by `$convert`'s `onError`). Covered
on both the Python and Rust servers (the Rust core computes the numeric cases
and defers string / Decimal128 parsing to Python).

#### Added

- `$toLong` — previously an unrecognized expression operator (code 168), now
  converts int / long / double (truncating toward zero) / bool / numeric string
  to a BSON `long`; a result outside `[-2^63, 2^63-1]`, or a non-finite double,
  overflows with code 241.
