### Whole-number-double acceptance completed for $substrCP, $range, and $round/$trunc

Finishes the whole-number-double sweep begun for the array-index operators. Like
mongod, `$substrCP` (start/length), `$range` (start/end/step), and the precision
argument of `$round`/`$trunc` now accept a whole-number double (coerced to the
integer) and reject a *fractional* one with mongod's exact per-argument code
rather than rejecting every non-integer. So `$range: [0.0, 5.0, 1.0]` yields
`[0,1,2,3,4]` on both servers, while `$range: [0, 5.7]` raises 34446.

The Python server carries mongod's codes — `$substrCP` 34451/34453, `$range`
34444/34446/34448, `$round`/`$trunc` place 51082 — and the Rust core coerces the
whole double (computing the same result) or defers the fractional case to
`BadValue`. As with the array operators, the coercion had to land on both engines
together so they never disagree on a valid whole-double argument. Three-way
mongod 7.0.12-verified.

#### Fixed

- `$substrCP`, `$range`, and `$round`/`$trunc` (precision) accept a whole-number
  double argument (coerced to int) and reject a fractional one with mongod's
  exact error code, instead of rejecting all non-integer values (both servers).
