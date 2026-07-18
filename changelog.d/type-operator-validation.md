### $type validates its argument and accepts whole-double codes

The `$type` query operator didn't validate its argument: an unknown alias, an
out-of-range or fractional numeric code, and a bool all silently matched nothing
instead of erroring, and the Rust engine additionally rejected a valid whole-double
code (`{$type: 2.0}`) that mongod accepts. mongod validates it: a known alias or a
numeric code in `{-1, 1..19, 127}` (a whole double counts) is valid; an unknown
alias or an out-of-range / fractional code is `BadValue` (2, with a `{$exists:
false}` hint for code `0`), and a bool / other type is `TypeMismatch` (14). Both
servers now match.

The Python server carries mongod's codes; the Rust core defers the invalid cases
so the Rust server rejects them too, and now computes whole-double codes rather
than deferring (so the Rust server no longer rejects a valid `{$type: 2.0}`). All
22 aliases (including the deprecated ones and `number`) are recognised. Three-way
mongod 7.0.12-verified.

#### Fixed

- `$type` rejects an unknown alias / out-of-range / fractional code (`BadValue`)
  and a bool (`TypeMismatch`) instead of silently no-matching, and accepts a valid
  whole-double numeric code on both servers (previously the Rust server rejected
  it).
