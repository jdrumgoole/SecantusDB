### $mod matches mongod on floats, bools, and error cases

A fourth query-operator bug from the driver-gauge triage, this time in `$mod`.
mongod truncates both the field value and the divisor toward zero to integers,
excludes bool (bool is not a number for `$mod`), and uses C-style truncated
modulo (sign of the dividend). Both servers diverged: they matched a bool
field (`{a: {$mod: [2, 1]}}` wrongly matched `a: true`), didn't truncate
non-integer floats, and Python's floored `%` disagreed on negatives — and the
Rust server outright errored (`BadValue`) on a double-valued field, aborting
the whole query. Now both engines truncate value and divisor, exclude bool,
compute C-style modulo, and raise mongod's errors for a zero divisor and a
malformed spec — verified three-way against a live mongod 7.0.12 probe.

#### Fixed

- `$mod` truncates float values and divisors toward zero, excludes bool, and
  uses C-style (truncated) modulo, matching mongod on both servers.
- The Rust server no longer errors on a `$mod` query against a double-valued
  field.
- `$mod` with a zero divisor or a malformed spec raises like mongod.
