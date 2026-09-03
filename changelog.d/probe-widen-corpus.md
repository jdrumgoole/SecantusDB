### Widening the probe corpus found thirteen crashes and six wrong values

`tools/probes/agg_expressions.py` had run tens of thousands of times against both servers. Its value list contained no infinity, no NaN, no signed zero, no numeric boundary, and none of MinKey / MaxKey / Binary / Timestamp / Regex / Code.

A value **class** that is absent is invisible in exactly the way a passing test is: it costs nothing and it proves nothing. Adding those classes took the sweep from 3,968 cases to 6,628 and immediately surfaced **thirteen crash-class bugs** — each an `internal server error` reachable from any query — plus six wrong values on a server the campaign had held at zero for days.

| | Before | After |
| --- | --- | --- |
| Crash-class bugs, Python server | 13 | **0** |
| Wrong values, Rust server | 6 | **0** |
| `agg_expressions.py` cases | 3,968 | **6,628** |

#### The crashes, by root cause

Two causes, both closed at the class level rather than per instance:

- **Python's decimal contexts trapped `InvalidOperation`** instead of producing a value. decimal128's own answer for `Inf + -Inf` is NaN, and for the trig series on a non-finite operand it is the operator's limit. Both `_DEC128_CTX` and `_DEC_TRIG_CTX` now trap nothing.
- **`math.ceil` / `math.trunc` / `datetime.fromtimestamp` raise on values BSON accepts.** `$ceil` of a double infinity, `$trunc` of NaN, and `$toDate` of `Int64(2**63 - 1)` (year 292278994, outside Python's `datetime`) all escaped as internal errors where mongod answers.

#### Wrong values

- **The accumulators did not unwrap a one-element array.** `{$sum: [[1]]}` answered 0 where mongod answers 1 and `{$max: [[1]]}` answered `[1]` where mongod answers 1 — five shapes across `$sum` / `$avg` / `$min` / `$max` / `$stdDevPop`, on *both* engines. `{$sum: [[1, 2]]}` sums the inner array; `{$sum: [[1], [2]]}` has two operands, both arrays, both ignored.
- **Integer rounding went through `f64`**, so `$trunc` and `$round` lost `9223372036854775807` entirely. Both now compute in integers.

#### Rules probed, not derived

- **NaN is never a domain error** — it answers NaN for every trig operator in both numeric types, including the range-limited ones where `-1 <= nan <= 1` is trivially false.
- **A decimal infinity takes the operator's limit**, which is a table rather than a series: `$atan` is pi/2 to all 34 digits, `$tanh` is exactly 1, `$cosh` is `Infinity` from either side. So the Rust server can answer these exactly despite having no decimal transcendentals.
- **Rounding up out of int64 is an error** (`51080`), not a widening to double.
- **A date outside int64 is `241`**, not a saturation — which is what Rust's `as i64` was silently doing for `{$toDate: 1e308}`.
- **Python's `%` is floor-based where Rust's truncates.** The identical `n - (n % scale)` truncates toward zero in Rust and away from it in Python, so `$trunc` of `-12345` at place -1 differed by 10 between the engines.

#### A mongod bug, deliberately not reproduced

`{$round: [-2147483648, -1]}` answers **positive** `2147483646` on mongod — an exact int32 wrap of the correct `-2147483650`. The same overflow is handled three different ways: *widened* for a positive int32, *wrapped* for a negative one, and *detected* (`51080`) at int64 width. `$trunc` on the same input is correct.

Both servers answer `-2147483650`. Reproducing the wrap would mean writing arithmetic known to be wrong to match a defect, and it is recorded in `tasks/backlog.md` §7 with the arithmetic rather than chased.

#### Also

The probe's clients now use `datetime_conversion="DATETIME_AUTO"`. Without it the default codec raises `InvalidBSON` while *decoding* a legitimate out-of-range date, killing the probe rather than reporting a divergence — and it did so identically for mongod, so it is the client that cannot cope, not either server.
