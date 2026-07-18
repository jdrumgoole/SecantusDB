### $pop / $position / $slice / $bit reject a bool argument, like mongod

A cluster of update-operator bugs from the same root cause as `$inc`/`$mul`:
Python's `bool` being an `int` subclass. `$pop: true` was treated as `$pop: 1`
(pop the last element) on both servers, and `$push` with `$position: true` or
`$slice: true` computed on the Rust server (insert at index 1 / keep 1) — all
of these are parse errors in mongod. Every one now rejects a bool argument:
the Python server reports mongod's exact codes (9 for `$pop`, 2 for
`$position` / `$slice` / `$bit`) and messages, and the Rust server surfaces
`BadValue`. `$pop` now also errors on a number other than ±1 (it silently did
nothing before). Found while triaging the driver-gauge update operators;
three-way mongod 7.0.12-verified.

#### Fixed

- `$pop`, `$push` `$position` / `$slice`, and `$bit` reject a bool argument
  instead of coercing it to 1 (both servers); `$pop` errors on a non-±1 value.
