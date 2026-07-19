### $dateAdd / $dateSubtract / $dateTrunc validate their integer arguments

The date-arithmetic operators mishandled a non-integer `amount` / `binSize`: a
whole double (`2.0`) was over-rejected, and a bool was silently coerced to `1`.
mongod accepts an integer or a whole double, and rejects everything else: a
fractional double / bool / non-numeric `amount` is `Location5166405` ("$dateAdd
expects integer amount of time units"), a non-integer `binSize` is
`Location5439017`, and a non-positive `binSize` is `Location5439018`. Both servers
now match.

The Python server carries mongod's codes; the Rust core (via a new `date_int`
helper) now accepts a whole-double argument rather than deferring — so the Rust
server no longer rejects a valid `amount: 2.0` — and defers the invalid cases.
Three-way mongod 7.0.12-verified.

#### Fixed

- `$dateAdd` / `$dateSubtract` accept a whole-double `amount` and reject a
  fractional / bool / non-numeric one (`Location5166405`); `$dateTrunc` accepts a
  whole-double `binSize` and rejects a non-integer (`Location5439017`) or
  non-positive (`Location5439018`) one (both servers).
