### $bucketAuto `granularity` rounds boundaries to preferred-number series

`$bucketAuto` now honours the `granularity` option (`R5`/`R10`/`R20`/`R40`/`R80`,
`E6`/`E12`/`E24`/`E48`/`E96`/`E192`, `1-2-5`, and `POWERSOF2`), rounding bucket
boundaries to the ISO preferred-number series exactly as mongod does — instead of
rejecting a valid series as unsupported. The rounding is **hex-exact against real
mongod 7.0.12**, including mongod's non-standard floating-point results (its `R5`
boundary at 6.3 is the double `6.300000000000001`, i.e. `63 * 0.1`).

The rounder and the boundary walk were ported verbatim from mongod's
`granularity_rounder_preferred_numbers.cpp` and `document_source_bucket_auto.cpp`
to both the Python server and the Rust core (which backs the Rust server), so the
two agree bit-for-bit and both match mongod. A `granularity` groupBy value must be
a non-negative number: a non-numeric value, a `NaN`, or a negative number is
rejected (mongod codes 40258 / 40259 / 40260 on the Python server). A
Decimal128-valued groupBy is deferred (the standing Decimal128 precision
limitation); the int/double path is complete.

#### Added

- `$bucketAuto` `granularity` boundary rounding for every preferred-number series
  and `POWERSOF2`, hex-exact to mongod 7.0.12 on both the Python and Rust servers.

#### Fixed

- A `$bucketAuto` `granularity` groupBy value that is non-numeric (40258), `NaN`
  (40259), or negative (40260) is now rejected with mongod's code instead of the
  previous blanket "unsupported" error.
