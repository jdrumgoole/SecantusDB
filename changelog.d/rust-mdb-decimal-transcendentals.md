### Rust MongoDB server: Decimal128 trigonometry answers as mongod does, to the last digit

The Decimal128 trigonometric and hyperbolic operators now differ from
mongod 8.2.11 only in the authorised last digit of a finite result (ours is
correctly rounded; mongod's Intel decimal library is 1–2 units off).
Everything else was measured and now matches, across 614 shapes. In the
expression harness, wrong values fell from 24 to 6 (five of them that
authorised digit) and wrong codes from 6 to 0.

#### Fixed

- `$sin`, `$cos` and `$tan` of any argument the format holds. Above
  `1E+300` the argument is reduced by Payne–Hanek against 6,650 digits of
  1/(2π), so `$sin` of `1E+6000` is a value, where it was 50989 past about
  `1E+1100`. Checked against mpmath: 366 huge and mid-range arguments, all
  correctly rounded, the slowest in 0.14 s in a debug build.
- `$sinh` and `$cosh` overflow to ±Infinity and `$tanh` saturates to ±1 for
  large arguments, where `1E+20` and up were 2 "not supported".
- Zeros carry mongod's exponent:
  - `$sin`, `$atan` and `$tanh` return the zero as it came;
  - `$tan`, `$asin` and `$sinh` move its exponent down 40;
  - `$acos` of 1 is `0E-6176`.
- Tiny results are padded to 34 digits within the exponent floor.
  `$sinh` / `$tanh` no longer underflow to 0 below `1E-80`. `$atanh` of an
  argument whose exponent is below -50 is the argument itself, as written.
- A result that rounds to one is written as mongod writes it:
  - `$cos` of an argument whose exponent is -53 or less, and `$cosh` up to
    `1E-40`, are `1.000000000000000000000000000000000`;
  - above those, `$cos`, `$cosh` and `$tanh` (from 40) answer a bare `1`.

Fixes RM-245
(tests/backlog/test_rm_found_2026_10_21b.py::test_decimal_hyperbolic_and_trig_edges,
which now asserts mongod's answer up to the authorised last digit).
