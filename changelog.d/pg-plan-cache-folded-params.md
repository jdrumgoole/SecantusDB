### The Rust PostgreSQL server no longer reuses a plan across a folded parameter

A prepared statement whose parameter sat inside an expression the planner
folds into a value -- `WHERE k = $1 % 100000` -- had its plan reused with the
raw parameter substituted, so a later execution returned the wrong row. This
was in `0.1.0-beta.3`.

#### Fixed

- `secantusd-pg` reuses a prepared statement's plan only when every parameter
  stands alone where its value is used: compared with a column in the `WHERE`
  (`k = $1`, `k IN ($1, $2)`), an item of an `INSERT`'s `VALUES`, the whole of
  `SET v = $1`, or one operand of `SET v = v + $1`. A parameter anywhere else
  (`k = $1 % 100000`, `k = -$1`, `$1::int2`, `BETWEEN`, `LIMIT $1`) is planned
  on every execution.
- The same reuse would have stored the wrong value for
  `INSERT ... VALUES ($1 % 1000)` and `UPDATE ... SET v = $1 % 1000`; that part
  was introduced after `0.1.0-beta.3` and never released.
