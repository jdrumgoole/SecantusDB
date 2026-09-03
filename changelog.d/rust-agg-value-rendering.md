### The last of the deferred operand errors, and three renderings that look like one

The [operand-error campaign](rust-operand-error-surface.md) took the Rust server's `agg_expressions` sweep from 908 divergences to 117. This closes it to **32** — and the remaining 32 are two deliberate deferrals, not defers standing in for ordinary argument complaints. That class is gone.

| | Campaign start | After the first pass | **Now** |
| --- | --- | --- | --- |
| `agg_expressions.py`, Rust server | 902 code + 6 message | 117 + 0 | **32 + 0** |
| `agg_expressions.py`, Python server | 50 code + 212 message | 28 + 80 | **0 + 71** |
| Wrong values, either server | 0 | 0 | 0 |

**The Python server now has zero code divergences on this sweep**, down from 50.

#### Fixed

- **The trigonometric domain errors** (`50989`) across `$acos` / `$asin` / `$atanh` / `$acosh` / `$sin` / `$cos` / `$tan`, each with its own range string.
- **`$mergeObjects`** names the offending value, and flattens an evaluated array: a field path resolving to an array *is* the operand list, so `{$mergeObjects: "$arr"}` over `[3, 1, 2]` reports "input 3" rather than naming the whole array.
- **Six arithmetic type guards**, no two alike — `$add` and `$multiply` name the first offender, `$divide` and `$mod` name both operands, `$subtract` inverts them into `can't $subtract X from Y`, and `$atan2` carries a different code per position (`51044` / `51045`).
- **The unknown-argument family** — eighteen operators, two sentences, eighteen codes that share nothing, plus the `n`-operator family and `$median` / `$percentile`'s IDL wording.
- **`$round` and `$trunc` had no arity check at all**: `{$round: [3, 1, 2]}` answered `3`.
- **`$range` was checking 64 bits, not 32** — it accepted a long and built a range of a trillion elements, answering `[]`.
- **`$substr*` and `$strcasecmp` coerce their operands** rather than requiring a string, using the same rule `$toLower` does.

#### Rules a reasonable guess gets wrong

- **NaN is not a domain error.** `{$tan: NaN}` answers NaN, for all three of sin/cos/tan; only the infinities are refused. Both engines spelled the guard `not isfinite(x)` — which rejects NaN, and no probe corpus had asked.
- **Three value renderings that look like one.** `$acos` converts to double first (`1.09951e+12`), `$range` keeps the integer (`1099511627776`), and a `Decimal128` keeps its own representation in both — `2.50` does not become `2.5`.
- **`$subtract`'s `Date` capitalisation is positional**, not per-type: `can't $subtract string from Date` but `can't $subtract date from int`.
- **A `Decimal128` is numeric.** Reporting a type error for it would be wrong; both engines defer on it for a different reason, and the guards had to be written to let it through.

#### The test that was right all along

Capitalising `Date` on both sides of `$subtract` was caught by `test_arithmetic_date_semantics`, which held the second-operand case. Three times this session an old expectation turned out to be a stale citation and the new measurement won; here both were right, for different positions. Re-probing rather than pattern-matching on "old test, new measurement" is the only thing that told them apart.

#### Also

The parity fuzz called `_rust_eval` bare and let a named error escape as an exception — it was written when the engine could only answer or defer, and *naming* mongod's error is a third outcome it had no branch for. It now compares the named error against the pure engine, which is what the rest of the suite already did.
