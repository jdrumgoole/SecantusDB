### Decimal128 values outside `f64`'s range no longer take the wrong branch

decimal128 spans `1E-6176` to `9.999…E+6144`; `f64` spans about `1E-308` to
`1E+308`. Eight operators on the Rust server asked their classifying questions —
*is this infinite? is this zero?* — of an `f64` rendering of the argument, and
that rendering saturates: a finite `Decimal128("1E+6144")` reads back as
`f64::INFINITY` and a finite `Decimal128("1E-6176")` as `0.0`. Each took the
branch for a special value and answered confidently.

Measured against mongod 8.2.11: the Rust server diverged on **79 of 180** cells
(18 operators × 10 extreme inputs) and the Python engine on **64 of 364**
(14 × 26). Both grids are now 0 apart from `$exp` in its middle range on the
Rust server, which needs a decimal exponential series.

#### Fixed

- **`$sqrt` of a large finite decimal answered `Infinity`.** It now computes in
  decimal — correctly rounded, which IEEE 754 requires of square root, with the
  decimal spec's ideal exponent so `$sqrt` of `4` is `2` and of `1E+6144` is
  `1.00000000000000000E+3072`. Verified against mongod on 75 values.
- **`$toDouble` of an out-of-range decimal returned `inf` / `0.0`** where mongod
  raises `241 ConversionFailure`. A decimal converts only when the double is
  normal; the boundary is IEEE's tininess-after-rounding cut at
  `2^-1022 − 2^-1076`, a quarter of a subnormal ULP below `f64::MIN_POSITIVE`
  (bisected against 8.2.11), so a value representable as a *subnormal* double is
  still a `241`.
- **`$toBool` of a decimal below `f64`'s range was `false`.** `1E-6176` is not
  zero, and is now `true`.
- **`$floor` / `$ceil` of a decimal needing more than 34 integer digits**
  returned the value; they are the decimal spec's `quantize`, so mongod answers
  `NaN`. `$trunc` / `$round` deliberately do not share the rule.
- **`$degreesToRadians` / `$radiansToDegrees` answered `Infinity` above
  `1E+309` and a zero below `1E-324`.** Both are now one correctly-rounded
  decimal multiply by mongod's own 34-digit constant, reaching the subnormal
  range: `1E-6176` radians is `5.7E-6175`.
- **`$exp` in the two regions that need no series**: `|x| ≥ 1E+5` is decided by
  sign alone (`Infinity`, or `0E-6176` at the minimum quantum), and `|x| ≤
  1E-40` is exactly `1`.
- **Arithmetic results outside decimal128's exponent range were refused**
  rather than clamped. They now follow the format's rules — overflow to
  `±Infinity`, and rounding to the minimum quantum on the way down, which is
  where subnormal results come from.

#### Fixed (Python server)

The same sweep run against the pure Python engine found the same family, plus
three of its own:

- **Three crashes.** `$degreesToRadians` / `$radiansToDegrees` of a decimal
  whose exact product falls outside decimal128's range raised a raw
  `decimal.Inexact` or `decimal.Overflow` out of the evaluator — an internal
  server error where mongod returns a value (`5.7E-6175`, `Infinity`).
  Constructing a `Decimal128` now clamps to the format instead of refusing.
- **The angle conversions computed `x * pi / 180`**, two roundings where mongod
  does one — the association the double path's own comment warns against. They
  now use mongod's 34-digit constants, which fixes results that came back with
  32 significant digits.
- **`$trunc` / `$round` of a decimal past 34 integer digits answered `NaN`.**
  They do not quantize; mongod expresses the value at the finest quantum that
  fits.
- Plus the shared items above: `$sqrt` of a negative decimal too small for
  `float` to keep the sign, `$toBool`, `$toDouble`, and `$floor` / `$ceil`.
