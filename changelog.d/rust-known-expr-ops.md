### `$stdDevPop` and `$stdDevSamp` work in an expression on the Rust MongoDB server

`{$stdDevSamp: [1, 2]}` in a `$project` / `$addFields` / `$set` answered
`168 Unrecognized expression` where mongod answers `0.7071067811865476`. The
`$group` accumulator forms were fine; only the expression forms were affected.

The evaluator had implemented both for three weeks, with unit tests. The names
were simply absent from `KNOWN_EXPR_OPS`, which the pipeline validator consults
*before* the evaluator runs — so a correct, tested engine sat behind a gate that
refused to let any pipeline reach it. The Python server was never affected.

#### Fixed

- Both operators are on the validator's list, so pipelines reach the evaluator
  that already implemented them. Verified against mongod 8.2.11: `$stdDevPop`
  over `[1, 2, 3]` is `0.816496580927726` and `$stdDevSamp` over `[1, 2]` is
  `0.7071067811865476` on both. Across the expression sweep this took the Rust
  server from 96 code divergences to 2, and those two are the documented
  Decimal128 `$atan2` / `$pow` deferrals.
- The list's drift guard actually guards something now. The old test looped
  over `KNOWN_EXPR_OPS` asserting a function whose entire body is a lookup in
  `KNOWN_EXPR_OPS` accepted each name — the list agreeing with itself, unable
  to fail however far the code drifted. The replacement reads `apply_op`'s
  match arms from the source and asserts every dispatched operator is listed;
  it was confirmed to fail, naming both operators, when the fix is reverted.
