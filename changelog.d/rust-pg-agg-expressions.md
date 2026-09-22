### The Rust PostgreSQL server computes with aggregate results, and pads `char(n)` where PostgreSQL does

`count(*) + 1` was refused. So were `sum(n) + 0` and `coalesce(sum(n), 0)` —
anything that did arithmetic on what an aggregate returned. The shape of the
plan was the reason: an output column named either a grouping key or an
aggregate, with no room for a computation over them.

#### Added

- Aggregates inside an expression. The aggregates within are extracted and
  planned as ordinary items — computed once per group as usual — and the
  expression then runs over their results, reading each from its own slot. An
  identical aggregate written twice in one expression is computed once.
- `char(n)` reaches `array_agg`, `min` and `max` blank-padded, which is what
  PostgreSQL does: those take the column's own type. `string_agg` does not, its
  argument being coerced to `text`, and the counting aggregates cannot tell the
  difference either way. Measured on PostgreSQL 14.24 rather than reasoned
  about — the three behave differently and the difference is not guessable.

#### Fixed

- `min(c) || '|'` over a `char(n)` no longer answers with the padding.
  `min`/`max` carry the column's `atttypmod` so the *encoder* pads on output;
  padding the stored value instead put blanks through `||`, which PostgreSQL
  strips. The first implementation did exactly that, and the concatenation
  case is what caught it.
