### Comparison operators accept every BSON type

`$cmp` / `$gt` / `$gte` / `$lt` / `$lte` / `$eq` / `$ne` — and `$expr`, which
routes through them — were gated on the Rust engine's `order::is_sortable`. That
predicate guards the **sort** engines, where a type lacking a transitive
same-type arm would corrupt an ordering, and it deliberately excludes NaN,
Binary, Timestamp, Regex, JavaScript and Min/MaxKey.

A single comparison needs no transitivity, and mongod compares every BSON type
by its canonical rank. Gating on the narrow predicate turned each of those types
into `2 … not supported by the Rust server`, so **one `BinData` document made an
entire `$expr` query fail**.

Measured against mongod 8.2.11 over 19 value classes × 3 operands × 7 operators:
**120 of 399 cells diverged**, now 0.

#### Fixed

- The comparison operators use a new `order::is_comparable`, which admits every
  type `order::cmp` ranks. `is_sortable` is unchanged, so the sort engines keep
  their narrower guarantee.
- **`order::cmp` placed NaN *equal* to every other number.** mongod ranks it
  below them — `{$cmp: [NaN, 5]}` is `-1` — and this project's own storage sort
  already did, so the comparison operators and the sort disagreed with each
  other. A unit test had pinned the `Equal` behaviour; it asserted Python's
  `<`-is-false-both-ways rather than anything measured.
- `order::cmp` gained the missing same-type arms for JavaScript code, which
  previously fell through to the numeric branch and compared equal.

The Python server was already correct here — its `_bson_lt` covers the wider set
— so this was a Rust-only gap, and it was found by sweeping the Rust server
against mongod rather than against the other engine.
