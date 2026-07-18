### $log rejects a non-numeric argument or base

`$log` type-checked neither of its operands: a string argument or base leaked a
raw Python `TypeError`, and a bool was silently coerced to `1` / `0`. mongod
rejects both — a non-numeric argument is `Location28756`, a non-numeric base is
`Location28757` — before the positive-domain check, while `null` still passes
through as `null`. Both servers now match, completing the math-operator type-guard
family (the unary ops landed in the previous slice).

The Python server carries mongod's codes; the Rust core defers these cases (bool
included, reusing the `math_float` helper) so the Rust server rejects them too.
Three-way mongod 7.0.12-verified.

#### Fixed

- `$log` rejects a non-numeric (incl. bool) argument (`Location28756`) or base
  (`Location28757`) instead of coercing a bool or leaking a Python `TypeError`
  (both servers).
