### $all validates its argument instead of silently mis-matching

`$all` accepted a malformed argument. A non-array leaked / mis-parsed, and a
`$`-expression element that wasn't the all-`$elemMatch` form — mixing `$elemMatch`
with a scalar, or using another `$`-operator document — was silently treated as an
equality clause (matching nothing) rather than erroring. mongod rejects both with
`BadValue`: "$all needs an array" and "no $ expressions in $all". Both servers now
match.

A pure-scalar `$all`, an all-`$elemMatch` form, regex elements, and plain
subdocument elements remain valid. The Python server carries mongod's `BadValue`;
the Rust core defers these cases so the Rust server rejects them too. Three-way
mongod 7.0.12-verified.

#### Fixed

- `$all` rejects a non-array argument ("needs an array") and a `$`-expression
  element outside the all-`$elemMatch` form ("no $ expressions in $all") with
  `BadValue`, instead of silently mis-matching (both servers).
