### $inc / $mul reject a non-numeric argument, like mongod

`$inc` and `$mul` silently computed with a bool argument — `{$inc: {n: true}}`
added 1 and `{$mul: {n: false}}` multiplied by 0, because Python's `bool` is an
`int` subclass — and the Python engine raw-raised a `ValueError`/`TypeError`
on a string or null argument instead of a clean coded error. mongod rejects
any non-number argument with `Cannot increment with non-numeric argument:
{field: value}` (code 14). Both servers now reject it: the Python server
raises mongod's exact message and code; the Rust server surfaces `BadValue`
(the standing update error-code gap), but neither silently computes a wrong
result. Found while triaging the driver-gauge update operators; three-way
mongod-verified.

#### Fixed

- `$inc` / `$mul` by a bool, string, or null argument is rejected instead of
  computing a wrong value (both servers); the Python server reports mongod's
  code 14 and message.
