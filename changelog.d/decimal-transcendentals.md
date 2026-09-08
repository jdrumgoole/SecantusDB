### `$ln`, `$log10`, `$exp` and `$asinh` compute on a finite Decimal128

These four used to refuse a finite non-zero decimal outright. On the Rust server
that is a `BadValue` — a deferral has no Python behind it there — so a
collection holding `Decimal128` values could not take a logarithm at all. They
now compute at decimal128's 34 significant digits, on both servers, from a new
arbitrary-precision layer in `secantus-core`.

#### Added

- A high-precision decimal core (`hp_mul` / `hp_add` / `hp_div` / `hp_sqrt`)
  that does the same arithmetic as the 34-digit operators at a caller-chosen
  width. The existing `add` / `mul` round to 34 digits at every step, which is
  right for arithmetic and useless for a series: an argument reduction can
  cancel thirty digits away and leave nothing behind.
- `ln` by argument reduction (`x = m·10^k`, `m = r·2^j`) onto an `atanh` series;
  `exp` by reduction onto a power of ten and a Taylor series; `log10` from
  `ln`, except for an exact power of ten, which answers the integer with no
  series at all — the only way `$log10` of `1E+400` comes out as `400`.
- Rounding is verified rather than assumed: the working precision widens
  (80 → 140 → 260 digits) until the guard digits actually decide the 34-digit
  answer, instead of trusting a fixed guard.

#### Fixed

- **`$asinh` of a small argument lost most of its digits on both engines.**
  `asinh(x) = ln(x + sqrt(x²+1))` cancels to `1 + x` for small `x`, so at any
  fixed precision the answer collapses: the Rust server returned `0` for
  `Decimal128("1E-100")` and the Python engine had eleven digits of error at
  `1E-10`. Below `1E-18` the correction term falls past all 34 digits and the
  answer is the argument itself; above it, Python now widens its working
  precision with the exponent.

#### Changed

- **These four are correctly rounded, which means they differ from mongod on
  about a fifth of finite inputs.** Over 290 measured pairs mongod is correctly
  rounded on 231; it carries Intel RDFP's approximation error in the last digit
  on the rest, and matching that would mean linking RDFP. `$ln` of
  `Decimal128("2.5")` is now `…117680111` where 8.2.11 answers `…117680110` —
  the true value is `…1176801107145…`. Measured, deliberate, and recorded in
  `tasks/backlog.md`.
