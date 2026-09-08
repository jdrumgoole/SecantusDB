### Accumulator spec errors depend on WHERE the operator appears

`$firstN`, `$lastN`, `$minN`, `$maxN`, `$median`, `$percentile`, `$topN` and
`$bottomN` all take a document spec. Given something else, mongod's code depends
on the operator's position — a `$group` output field is *accumulator* position
and an `$addFields` value is *expression* position, and they do not agree:

```
{$group:     {x: {$median: 5}}}   ->  7436100
{$addFields: {x: {$median: 5}}}   ->  7436201
```

Both servers carried one table and applied it in both positions, which was right
for four of the eight operators and wrong for four. Measured across all eight ×
{scalar, array} × {accumulator, expression} against mongod 8.2.11: **16 of 32
cells diverged**, now 0.

#### Fixed

- **Accumulator position has its own codes** — `$median` 7436100, `$percentile`
  7429703, `$topN` / `$bottomN` 5788001, where the expression table says 7436201
  / 7436200 / 168.
- **An array spec in accumulator position is one message for all eight**:
  `40237 The <op> accumulator is a unary operator`, not the per-operator
  "specification must be an object".
- **`$topN` / `$bottomN` are accumulator-only.** As an expression mongod does
  not recognise the name and never looks at the spec, so it answers
  `Unrecognized expression '$topN'`. A unit test had asserted the object-spec
  wording here and passed, because "Unrecognized expression" carries code 168
  too and only the code was ever checked.
- **`$median` / `$percentile` are validated in mongod's IDL field order** —
  `input`, then `p`, then `method`. Checking `method` first made `{$median: {}}`
  name `method` where mongod names `input`, and let a bad `method` outrank a bad
  `p`. The Python side's ordering came from a 7.0.12 probe.
- **The bad-method message was missing an article**: mongod says "used as **a**
  percentile 'method'".
- **`$percentile`'s `p` has three distinct codes, not one.** A non-array or an
  EMPTY array is 7750301 naming the array, a non-number element is 7750302, and
  a number outside `[0, 1]` is 7750303. An empty `p` was accepted outright and
  produced an empty result; elements rendered as `a` and `None` rather than
  mongod's `"a"` and `null`.
- **On the Rust server the whole family deferred**, which surfaces as
  `2 aggregation pipeline uses a stage or operator not supported by the Rust
  server` — blaming the operator for a bad argument. Its `$median` /
  `$percentile` accumulator and expression forms now share one validator.
- **These errors are raised at PARSE time, which is what fixes the wrapper.**
  mongod reports them while parsing, so `$addFields` prefixes
  `Invalid $addFields :: caused by ::` and `$group` prefixes nothing at all.
  Raising them from the evaluator instead produced `Failed to optimize pipeline`
  on one server and `Executor error during aggregate command` on the other —
  right code, right sentence, wrong wrapper, which no code comparison can see.

#### Note

Adding the accumulator-only rule to the shared expression walker initially
rejected every valid `{$group: {x: {$topN: {…}}}}` — the walker recursed into
the accumulator and did not know which stage it was in. The stage name is now
threaded through the whole walk rather than passed only at the top.
