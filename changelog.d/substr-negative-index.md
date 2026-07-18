### $substr* reject a negative index, like mongod

A negative start (or, for `$substrCP`, a negative length) silently produced a
Python-style negative-index slice — usually an empty or wrong substring — instead
of the error mongod raises. Both servers now reject: the Python server with
mongod's exact codes, the Rust core defers to `BadValue`.

- `$substrBytes` / `$substr` negative start → **50752** ("starting index must be
  non-negative"). A negative *length* is still fine — it means "to the end".
- `$substrCP` negative start → **34455**, negative length → **34454**.

This completes `$substrBytes` / `$substrCP` numeric-argument fidelity (bool
rejection, byte-vs-code-point aliasing, whole-double / fractional handling,
UTF-8-split rejection, and now negative indices). Three-way mongod 7.0.12-verified.

#### Fixed

- `$substrBytes` / `$substr` / `$substrCP` reject a negative start (and
  `$substrCP` a negative length) with mongod's exact error code instead of
  returning a Python-style negative-index slice (both servers).
