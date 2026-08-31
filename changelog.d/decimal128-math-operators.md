### Decimal128 through the math operators: 13 crashes, and half the digits

`Decimal128` is one of the types this project keeps documents as opaque BSON to
preserve. The aggregation math operators were converting it to `float`, or
failing outright: **13 of them crashed** the Python server with `internal server
error`, because `math` rejects a `Decimal128` and the `TypeError` escaped.
The rest silently narrowed 34 significant digits to 17.

Measured against mongod 8.2.11: **21 of 49 shapes correct before, 38 after.**

#### Fixed

- **The 13 crashes are gone.** `$abs`, `$ceil`, `$floor`, `$trunc`, `$round`,
  `$exp`, `$ln`, `$log10`, `$sqrt`, `$mod`, `$pow`, `$log` and `$avg` now compute
  in `decimal` at decimal128's 34-digit precision and return a `Decimal128`.
- **The hyperbolics keep full precision** — `$sinh`, `$cosh`, `$tanh`, `$asinh`,
  `$acosh`, `$atanh` are exact identities over `exp` / `ln` / `sqrt`, which
  `decimal` provides.
- `$degreesToRadians` / `$radiansToDegrees` use a decimal π instead of the
  float one.
- `$mod` uses `Decimal`'s truncate-toward-zero, which is mongod's rule; Python's
  `%` floors, so widening the existing expression would have been wrong for
  negative operands.
- `$pow` computes `exp(e * ln(b))`, not `b ** e`. That looks like the worse
  choice and is the right one: `2.5 ** 2` is exactly `6.25`, and mongod answers
  `6.249999999999999999999999999999999`.

#### A finding worth keeping: do not be more accurate than the reference

Computing the hyperbolic identities with **guard digits** — wide, then rounded
back to 34 — is more accurate and matched mongod **less**, moving `$cosh` from
agreeing to differing in the final digit. mongod accumulates its own rounding at
decimal128 precision throughout, so fidelity means reproducing that arithmetic
rather than improving on it. The extra precision was reverted.

#### Still open (recorded)

The **circular** functions — `$sin`, `$cos`, `$tan`, `$asin`, `$acos`, `$atan`,
`$atan2` — have no identity over the operations `decimal` provides and would need
series expansions; they still narrow to `float`. `$acosh` agrees to 33 of 34
digits. And `cmp(string, Decimal128)` is still inverted in the cross-type order.

27 cases added to `tests/test_mongod_differential.py`.
