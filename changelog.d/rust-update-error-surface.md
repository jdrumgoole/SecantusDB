### Update errors the Rust server named wrongly, or not at all

Two defects, measured against mongod 8.2.11.

**`$inc` / `$mul` with a non-numeric operand carried a wrapper mongod does not
send.** mongod has two shapes for the same code 14 and wraps only one:

```
{$inc: {n: "x"}}    operand bad, readable from the spec
    mongod  Cannot increment with non-numeric argument: {n: "x"}
    before  Plan executor error during update :: caused by :: Cannot increment ...

{$inc: {s: 1}}      stored field bad, needs the document
    mongod  Plan executor error during update :: caused by :: Cannot apply $inc ...
```

`arith_type_error` now returns `(message, exec)` and the storage layer threads
it through, instead of hard-coding every code 14 as execution-time.

**`$position` / `$slice` / `$bit` with a bool argument answered the generic
refusal.** The guards were already there and the code (2) was already right —
they simply deferred, which on this server reads as "the operator is not
supported" when the argument was at fault. `$position` and `$slice` are worded
*differently* by mongod ("not of type:" vs "but was given type:"), so each is
measured rather than shared.

#### Also: two backlog entries were stale

The query matcher's three "still deferred where faithful" residuals — an
exotic-text value under a collation, an exotic type range-compared inside an
array, and a `Decimal128`-valued `$mod` field — all **match mongod now** (0 of 6
shapes). Both entries are marked resolved rather than left advertising finished
work as remaining.

#### Fixed

- `secantus-core`: `arith_type_error` reports which of mongod's two wrappers
  applies; `$position` / `$slice` / `$bit` bool arguments carry mongod's text.
- `secantus-storage` / `-storage-adapter`: the `exec` flag is threaded instead of
  assumed.
