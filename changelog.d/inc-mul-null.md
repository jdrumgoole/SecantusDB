### `$inc` / `$mul` on an explicit-null field now errors like mongod

Applying `$inc` or `$mul` to a field that is present with an explicit `null`
value now raises a `TypeMismatch` (error code 14), exactly as real MongoDB
does — "Cannot apply $inc to a value of non-numeric type … of non-numeric type
null". Previously both servers silently coerced the null to `0` and applied the
delta, so `{$inc: {n: 5}}` against `{n: null}` returned `{n: 5}` instead of
failing. A *missing* (absent) field is still treated as `0` and the operation
applied — that has always matched mongod and is unchanged.

The fix distinguishes an absent field from a present-but-null one: the pure-Python
engine raises the coded error directly, and the Rust core defers the null case to
the Python oracle so the exact error code is preserved (the Rust server surfaces
a generic `BadValue`, the documented error-code gap).

#### Fixed

- `update.py` / `secantus-core`: `$inc` / `$mul` on a field present with an
  explicit `null` now errors with code 14 (`TypeMismatch`) instead of coercing
  the null to `0`. A missing field is still treated as `0` and the operation
  applied.
