### Date and misc aggregation operators match mongod's error codes

Continuing the operator error-code sweep, a set of date and miscellaneous
aggregation operators now raise mongod 7.0.12's exact error code instead of a
generic `TypeMismatch` (14) — and two more **silent accepts** are closed
(`$dateToString` on a non-date, and `$dateDiff` with a missing `endDate`
parameter, both of which returned a value where mongod errors). Both the Python
and Rust servers are fixed (the Rust core defers each case — `$dateToString`
and `$dateDiff` needed Rust-side fixes to stop computing `null`).

#### Fixed

- `$dateToString` / `$dateToParts` on a non-date `date` → `Location16006`
  (`$dateToString` was a silent `null`; a `null`/missing date stays valid).
- `$dateFromString` with a non-string `dateString` → `ConversionFailure` (241).
- `$dateAdd` / `$dateSubtract` / `$dateTrunc` with an unknown `unit` → code 9
  ("unknown time unit value").
- `$dateDiff` with a missing `startDate`/`endDate`/`unit` **parameter** →
  `Location5166303`/`5166304`/`5166305` (was a silent `null` for a missing
  `endDate`; a present-but-null parameter still yields `null`).
- `$let` referencing an undefined variable → `Location17276`.
- `$switch` with no branches → `Location40068`.
- `$ifNull` with fewer than two arguments → `Location1257300`.
- `$getField` / `$setField` with a non-string `field` → `Location5654602` /
  `Location4161107`.
- `$sortArray` with an invalid `sortBy` → `Location2942507`.
- `$convert` with a missing `input`/`to` parameter → code 9.
