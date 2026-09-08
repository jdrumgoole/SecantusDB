### Decimal128 transcendentals, measured over 96 pairs instead of guessed

This backlog entry was rewritten three times in one day, and the first two
rewrites were wrong because each inferred a *mechanism* from four or five data
points. A 96-pair sweep (12 operators × 8 inputs) settles it:

| rule | pairs |
| --- | --- |
| correctly-rounded **decimal128** | **76** |
| matches a **binary128** round-trip instead | 7 |
| neither — mongod 1–2 ULP off the correctly-rounded value | 13 |

`$sqrt` and `$asinh` are correctly-rounded decimal on **8 of 8** — IEEE 754
requires it for square root — so those are exactly reproducible today with no
dependency. Everything else is mixed.

The strays are real, not harness error; the reference series is stable at 60, 130
and 190 digits (`sin(2.5)` is 1 ULP low, `tan(2.5)` 2 ULP, `cos(2.5)` exact).

So implementing correctly-rounded decimal transcendentals in pure Rust is a
legitimate option — ~79% exact, `$sqrt`/`$asinh` fully exact — rather than the
trap an earlier write-up called it. The alternatives (link Intel RDFP; keep
refusing) are unchanged.

The sweep is kept as `tools/probes/decimal_transcendental_rule.py` so the next
session measures rather than re-theorises.
