### Finishing the enum story

The previous batch fixed enum comparisons in a `WHERE` clause by rewriting a
range comparison into the set of labels that satisfy it. A comparison in the
SELECT **list** has to yield a boolean instead, and is evaluated by the scalar
evaluator, which has no catalog — so `SELECT m > 'ok'` still answered by
*spelling* while `WHERE m > 'ok'` did not. Two halves of one operator
disagreeing is a worse state than either being wrong on its own.

#### Fixed

- An enum comparison in a projection. The planner stamps the declared label
  list on the comparison node for the evaluator to read, at the umbrella
  planner so every single-table shape it dispatches to is covered.
- `enum_range()`, `enum_first()` and `enum_last()`. They take their enum type
  from the **argument's cast** — the argument is a NULL — so they cannot go
  through the value-only builtin table and were `0A000`. An unknown type is
  `42704`.

A plain `text` column still compares by spelling, which is what PostgreSQL
does; only a column whose declared type is an enum is reordered.
