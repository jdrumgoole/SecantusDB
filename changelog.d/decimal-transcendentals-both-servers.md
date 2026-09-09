### The Rust server answers decimal trigonometry instead of refusing it

Ask the Rust server for `{$sin: <Decimal128>}` — or `$cos`, `$tan`, `$asin`,
`$acos`, `$atan`, `$sinh`, `$cosh`, `$tanh`, `$acosh` — and it used to reply
"a construct the Rust server does not support". MongoDB returns a number. That
is the least faithful outcome available, and it covered 108 of 285 measured
shapes. All ten now answer, sharing the Python engine's results.

Getting there settled a question that had been treated as a matter of taste.
Compared against a 60-digit reference, **MongoDB is correctly-rounded only about
78% of the time** for these operators — it carries Intel's decimal-library error
in the last digit. Exact agreement is therefore capped, and no choice of working
precision reaches it. What follows is the practical part: being *correct* is the
closest we can get to MongoDB, because wherever we round correctly our agreement
equals MongoDB's own accuracy.

That condemned an older strategy in the Python engine, which computed the
hyperbolics at 34 digits throughout in order to reproduce MongoDB's
accumulation. It had been adopted on the strength of one `$cosh` case:

| | agreement at 34 digits | computed wide |
| --- | --- | --- |
| `$tanh` | 3/20 | **12/20** |
| `$sinh` | 8/20 | **12/20** |
| `$acosh` | 12/15 | **14/15** |
| `$cosh` | 16/20 | 16/20 |

`$cosh` did not even lose, so the case behind the strategy did not support it.

#### Fixed

- **The Rust server implements the whole decimal trig and hyperbolic family.**
  `sinh` / `cosh` / `tanh` / `acosh` / `atanh` from the existing high-precision
  `exp` / `ln` / `sqrt`; `atan` / `asin` / `acos` from an argument-reduced Taylor
  series; `sin` / `cos` / `tan` by reduction modulo an embedded 2π. Zero
  refusals, and identical to the Python server throughout.
- **The Python hyperbolics compute with guard digits and round once.**
  `$tanh`, `$sinh` and `$acosh` were losing accuracy — and agreement — to
  compounded 34-digit rounding.

Agreement with mongod 8.2.11 rose from 209 to 224 of 285 on the Python server
and from **90 to 224** on the Rust server. The remaining 61 are MongoDB's own
last-digit error.

#### Added

- `tools/probes/decimal_transcendental_rounding.py` — the only probe here that
  asks "is the answer *right*?" alongside "does it match MongoDB?", against an
  mpmath reference. That pairing is what distinguishes our bug from MongoDB's
  error, and it fails the run only on the former.

#### Known gap

`sin` / `cos` / `tan` on the Rust server reduce against 2π embedded to ~1200
digits, so an argument beyond about 1e1100 still defers. MongoDB carries π to
the full decimal128 range.
