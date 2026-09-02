### Exact decimals, NaN's place in the order, and one message hiding five bugs

The Rust PostgreSQL server could not compare a great many pairs of values, and
said so with a single message — "comparing these operands" — that named neither
of them. Making that message name the types turned one entry on the failure list
into five distinct causes, four of which were the same rule and none of which
were guessable from the text.

**Arithmetic on decimals had stopped working entirely.** When decimal literals
became exact numerics rather than floating-point numbers in the previous
release, every arithmetic operator on them began refusing outright: `1.5 + 1.5`
was an error. It is fixed here, and fixed exactly — `0.1 + 0.2` is `0.3`, and the
result's *scale* is part of the answer, so `1.50 + 1.5` is `3.00` where
`1.5 + 1.5` is `3.0`. Division stays refused rather than guessed at, because its
result scale depends on the operands in a way that has not been measured.

**Comparing decimals no longer goes through a floating-point number.** A numeric
carries 34 significant digits and a float holds about 15, so two visibly
different twenty-digit numbers were the same float — and compared equal.

**NaN has a place in PostgreSQL's ordering**, which the IEEE rules it inherits
from do not give it: NaN equals itself and sorts above every number, infinity
included. The underlying comparison reports "no answer" for each of those, which
this server passed on to the client as an error where PostgreSQL has a result.

**An unknown literal takes the type of the operand beside it** — in comparisons
just as in arithmetic. That type then decides both the parse and the error, which
is why comparing an interval to `'2020-01-01'` reports a bad interval rather than
`false`. Implementing this rule for arithmetic alone in the previous release left
four of the five failures above.

#### Added

- Exact `+`, `-` and `*` on decimals, with PostgreSQL's result scales.
- Comparison of decimals (on their digits), timestamps, and NaN / infinity.

#### Fixed

- Arithmetic on decimal literals refused outright since they became exact.
- Two different numerics with more digits than a float can hold compared equal.
- An unknown literal beside a typed operand was not resolved for comparison.
- The "cannot compare" error now names both operand types.
