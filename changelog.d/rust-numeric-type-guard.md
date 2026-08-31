### `$abs only supports numeric types, not string` — the Rust engine's largest error family

The biggest single block left in the expression sweep: **24 unary operators, 220
shapes** where mongod answers `28765 $OP only supports numeric types, not
<type>` (or `51081` for `$round` / `$trunc`) and the Rust server answered its
generic `BadValue` (2).

The Python server already had these right, so this is a Rust-side gap: its
operators know the operand's type when they evaluate it, but their only failure
signal is `Fallback`, which carries no code — the comment on `op_abs` read
"Python raises 28765 -> defer", which is true on a server that has Python and
useless on one that does not.

#### Fixed

`$abs`, `$acos`, `$acosh`, `$asin`, `$asinh`, `$atan`, `$atanh`, `$bitNot`,
`$ceil`, `$cos`, `$cosh`, `$degreesToRadians`, `$exp`, `$floor`, `$ln`,
`$log10`, `$radiansToDegrees`, `$round`, `$sin`, `$sinh`, `$sqrt`, `$tan`,
`$tanh`, `$trunc` now answer mongod's code and sentence on the Rust server, for
a constant operand and for a field reference alike.

It works by re-evaluating just the **argument** — not the operator — against the
documents the stage sees, which is enough to name the error without widening
`Fallback`. That is the `update::arith_type_error` template the other validators
in this module use. A null operand is not an error, and `Decimal128` is numeric:
it defers for a different reason, so reporting a type guard for it would be wrong.

#### A note on the gate

`may_name_runtime_error` decides whether to keep a copy of the input documents
for this naming pass, and it is deliberately narrow because the copy is taken on
the success path too. It now also fires when a numeric-guard operator appears
anywhere in a stage — a spec scan, so it costs nothing for a pipeline without one.

#### Sweep status, and one thing that got *more* visible

Rust code differences **1556 → 1336**. Rust message-only went 0 → 148, which is
these same cases moving from "wrong code" to "right code, wrong wrapper": for a
**constant** operand mongod folds at optimization time and says
`Failed to optimize pipeline :: caused by ::` where we say
`Executor error during aggregate command on namespace: … :: caused by ::`.

That is the long-deferred constant-folding item — but it is now measured rather
than guessed, and the rule turns out to be **statically decidable**: mongod folds
exactly when the argument contains no field-path reference (`$$NOW` folds too).
Recorded in `tasks/backlog.md` with the probe, since it is a much cheaper
proposition than "modelling constant folding" implied.

14 differential cases added.
