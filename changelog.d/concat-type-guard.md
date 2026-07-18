### $concat rejects non-string operands instead of coercing them

`$concat` silently `str()`-coerced any operand — `{$concat: ["x=", 5]}` produced
`"x=5"` — and treated a null / missing operand as an empty string. mongod requires
every operand to be a string: a non-string operand is `Location16702` ("$concat
only supports strings, not <type>"), and a null or missing operand short-circuits
the whole expression to `null` (evaluated left-to-right, so a non-string that
precedes a null still errors). Both servers now match.

The Python server carries mongod's code; the Rust core defers a non-string operand
(so the Rust server rejects it) and now returns `null` on a null operand rather
than skipping it. Three-way mongod 7.0.12-verified.

#### Fixed

- `$concat` rejects a non-string operand with `Location16702` and returns `null`
  for a null / missing operand, instead of `str()`-coercing operands or treating
  null as an empty string (both servers).
