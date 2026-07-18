### $densify validates its range spec

The `$densify` stage didn't validate its `range`. A date `unit` applied to a
numeric field leaked a raw Python `TypeError` (adding a `timedelta` to an int), a
bool `step` was silently coerced to `1`, a non-positive `step` and malformed
`bounds` (a bad string, a wrong-length array, a descending array) were quietly
accepted or mis-handled. mongod rejects each with a specific code: a numeric value
under a date unit is `6053600`, a bool step is `14`, a non-positive step is
`5733401`, a bounds string that isn't `"full"` / `"partition"` is `5946802`, a
bounds array that isn't exactly two elements is `5733403`, and a non-ascending
bounds array is `5733402`. A fractional `step` (`1.5`) is still accepted. Both
servers now match.

The Python server carries mongod's codes; the Rust core defers every invalid case
(bool step included) so the Rust server rejects them too. Three-way mongod
7.0.12-verified.

#### Fixed

- `$densify` rejects a date unit on a numeric value (`6053600`), a bool step
  (`14`), a non-positive step (`5733401`), and a malformed `bounds` string /
  array (`5946802` / `5733403` / `5733402`), instead of leaking a Python
  `TypeError`, coercing a bool, or silently mis-handling the range (both servers).
