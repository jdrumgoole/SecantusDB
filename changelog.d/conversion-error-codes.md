### mongod-specific error codes for conversion / string-length / sort expressions

Several aggregation expressions raised a generic `TypeMismatch` (code 14) where
real mongod returns a specific error code. They now match mongod 7.0.12, so a
`pymongo` client sees the same `code` on a failed operation. This is a Python
server refinement — the Rust server already surfaced `BadValue` for these.

#### Fixed

- `$toInt` / `$toLong` / `$toDouble` / `$toDecimal` on an unparseable numeric
  string now raise `ConversionFailure` (241) instead of 14.
- `$convert` with an unknown target type name now raises code 2
  ("Unknown type name: …") and is **not** swallowed by `onError` (a query-compile
  error, matching mongod), instead of 14.
- `$sortArray` on a non-array input now raises `Location2942504` instead of 14.
- `$strLenCP` / `$strLenBytes` on a non-string argument now raise
  `Location34471` / `Location34473` instead of 14.
