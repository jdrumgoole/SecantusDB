### Conversions, decimal `$divide` / `$mod`, and missing-parameter errors

Re-running the 6,628-case expression corpus against the Rust server found 28
shapes answering a different code from mongod. Extending the same cases to the
pure Python engine found the same family plus three of its own. The grid of
mongod × Rust server × Python engine over these 76 cases is now 0 divergent.

#### Fixed

- **`$dateFromString`, `$dateToParts`, `$dateTrunc` and `$dateDiff` answered
  `null` for a missing required parameter** where mongod raises. `{$dateTrunc:
  {}}` was `null`, not an error — a wrong value, not a wrong message. The codes
  share no pattern (40542 / 40522 / 5439009 / 5166303) so they are a measured
  table, not a rule.
- **`$getField` named the wrong missing field.** mongod checks `field` before
  `input`, the reverse of the order it reads them in, so `{$getField: {}}` is
  `3041702` and not `3041703`.
- **`$divide` by a decimal zero raised `decimal.DivisionByZero` out of the
  Python evaluator** — an internal server error. `b == 0` is `False` for
  `Decimal128("0")`, since the BSON wrapper defines no comparison against `int`,
  so a decimal zero divisor walked past the guard.
- **`$mod` of `Decimal128("1E+6144")` by `7` answered `NaN`.** The quotient
  needs 6,145 digits and `Decimal.__mod__` raises `InvalidOperation` past the
  working precision. It is now computed at whatever width the quotient needs,
  and the Rust side does it as an exact integer remainder.
- **`$mod` by zero used the wrong code with a decimal operand**: 16610 is for
  int / double, and it is 5733415 once a decimal is on either side.
- **`$toDate` of a fractional value did not truncate.** A BSON date holds whole
  milliseconds, so `{$toDate: 1.5}` is 1ms; the Python engine built a datetime
  with 1500 microseconds, a value BSON cannot hold.

#### Added

- **`$divide` and `$mod` accept `Decimal128` on the Rust server**, which
  refused them outright. `$divide` carries the decimal spec's ideal exponent, so
  `100 / 10` is `10`, `2.50 / 1.0` is `2.5` and `1 / 8` is `0.125`; `$mod` takes
  its sign from the dividend and its quantum from `min(e1, e2)`, so `7.5 % 2.5`
  is `0.0`.
- **binData conversions on both servers.** `$toInt` / `$toLong` reinterpret the
  bytes as a *little-endian* integer (`BinData(0, "01020304")` is `67305985`,
  not `16909060`), `$toDouble` reinterprets 4 bytes as an IEEE single and 8 as a
  double, and `$toString` is base64. Each target accepts its own set of lengths
  and names the rest.
- **`$toDecimal` of `inf` / `-inf` / `nan`**, which was a `241`.

#### Fixed (regression from this session)

- **`$asinh` of a decimal below `1E-4966` returned the value where mongod
  underflows to a bare `0`.** mongod's own implementation gives up there — and
  gives progressively wrong answers for a decade above it, `$asinh(1E-4965)`
  being `1.295…E-4965` where the true value is `1E-4965`. This server had been
  changed to answer the mathematically correct value because the two engines
  were compared to *each other* rather than to mongod. The Rust server's
  exemplar is mongod; the threshold is bisected from 8.2.11 and both servers now
  follow it.
