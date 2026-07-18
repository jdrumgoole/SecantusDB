### Aggregation expressions reject a bool where a number is expected, like mongod

The bool-as-int cluster reaches the aggregation expression engine. Because
Python's `bool` is an `int` subclass (and the Rust core mapped `Boolean`
straight to 0/1), a bool argument slipped through the numeric checks of eight
operators and was *computed* instead of *rejected*: `$round`/`$trunc` treated
`true` as a decimal place, `$arrayElemAt`/`$slice`/`$indexOfArray`/`$substrCP`
treated it as an index, and `$sortArray` as a sort direction. Every one is a
parse error in real mongod — a bool is not a number — and both servers now say
so.

The Python server reports mongod's exact per-operator codes (`$round`/`$trunc`
16004, `$arrayElemAt` 28690, `$slice` 28725/28727, `$sortArray` 2942507,
`$substrCP` 34450/34452, `$range` 34443/34445/34447, `$indexOfArray` 40096) and
messages; the Rust server surfaces `BadValue`. Found while sweeping the
aggregation surface for the same root cause as the `$inc`/`$mul` and
`$pop`/`$position`/`$slice`/`$bit` clusters; three-way mongod 7.0.12-verified.
`$range` already rejected a bool (with a generic code) and now carries mongod's
per-argument code.

#### Fixed

- `$round`, `$trunc`, `$arrayElemAt`, `$slice`, `$sortArray`, `$substrCP`,
  `$range`, and `$indexOfArray` reject a bool argument with mongod's exact
  error code instead of coercing it to 0/1 (both servers).
