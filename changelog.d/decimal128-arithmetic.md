### Decimal128 arithmetic is exact on both servers

Stored decimals were quietly losing precision. `$inc`, `$mul`, `$sum` and `$avg`
ran in Python's default decimal context, which carries 28 significant digits —
but Decimal128 carries 34. Every arithmetic result on a decimal field was
silently truncated by six digits: incrementing
`1.000000000000000000000000000000001` by one answered
`2.000000000000000000000000000`, dropping the trailing digit a real MongoDB
server keeps. Nothing errored and nothing warned; the value simply came back
shorter than it went in. The same four operators failed outright on the Rust
server, which had no decimal arithmetic at all and rejected the write or the
pipeline rather than answering.

Both servers now compute decimals exactly. The Rust engine gained a
sign/coefficient/exponent implementation that adds, multiplies and divides
without an intermediate binary float, rounding half-even to 34 digits exactly
once and only when a result genuinely needs it. Crucially it preserves the
*quantum*: Decimal128 distinguishes `5.00` from `5`, so `2.50 + 0.10` is `2.60`
and `2.50 * 2` is `5.00`, matching MongoDB rather than collapsing to a
normalised form.

Getting there turned up a genuine MongoDB quirk worth knowing about: the server
uses two different rules for turning a double into a decimal. The accumulators
take the double's exact binary value, so `$sum` of `0.1` contributes
`0.1000000000000000055511151231257827`, while `$inc`, `$mul` and `$toDecimal`
round to 15 significant digits and contribute `0.100000000000000`. Both rules
are now reproduced on both servers, `$toDecimal` included — it had been using
neither. All of this is verified against a live mongod 6.0.16 rather than
asserted from documentation, and the two engines are pinned to each other by
several hundred thousand randomised comparisons.

#### Fixed

- `$inc` / `$mul` / `$sum` / `$avg` no longer truncate Decimal128 results to 28
  significant digits; all 34 are kept, with IEEE 754-2008 preferred exponents so
  the quantum survives (`2.50 + 0.10` → `2.60`, `2.50 * 2` → `5.00`).
- The Rust server computes these four operators over Decimal128 instead of
  failing. Previously a `$group` over a collection containing a single decimal
  value failed the whole pipeline, and `$inc` on a decimal field returned a
  write error.
- `$toDecimal` and `$convert: {to: "decimal"}` convert a double at MongoDB's 15
  significant digits (`0.1` → `0.100000000000000`, `4.125` →
  `4.12500000000000`) rather than its shortest round-trip text, and round from
  the exact binary value so denormals match (`5e-324` →
  `4.94065645841247E-324`). All four implementations — both operators on both
  servers — were previously wrong, each in its own way.
- `$sum` / `$avg` convert a double by its exact binary value, matching MongoDB's
  separate accumulator rule.

#### Added

- `crates/secantus-core/src/decimal.rs` — exact decimal128 arithmetic
  (`add` / `mul` / `div_int`, parse and render, both double-conversion rules).
- Decimal arithmetic cases in `tests/test_mongod_differential.py`, plus corpus
  cases in the update and aggregate Rust/Python parity suites.
