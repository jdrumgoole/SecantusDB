### $pow no longer crashes on a negative base with a fractional exponent

`$pow` with a negative base and a fractional exponent (e.g. `$pow: [-2, 0.5]`)
produced a Python **complex** number, which is unencodable — it crashed BSON
serialization of the response. It now returns `NaN`, matching mongod. `$pow` also
now validates its operands like mongod: a non-numeric base raises 28762, a
non-numeric exponent (including a bool) raises 28763, and a zero base with a
negative exponent raises 28764 — instead of silently coercing a bool, or leaking
a raw Python `TypeError`/`ZeroDivisionError`. Both servers; the Rust core already
returned `NaN` for the complex case (`f64::powf`) and now defers the bool /
zero-negative-exponent cases so both servers agree. Three-way mongod 7.0.12-verified.

This is the first fix from a **parallel divergence sweep** (recorded in
`tasks/divergence-catalog.md`) that probed the full operator surface against real
mongod and turned up a queue of type-coercion / argument-validation gaps.

#### Fixed

- `$pow` returns `NaN` for a negative base with a fractional exponent instead of
  crashing BSON encode, and rejects a non-numeric / bool operand (28762 / 28763)
  or a zero base with a negative exponent (28764) instead of coercing or leaking
  a raw Python exception (both servers).
