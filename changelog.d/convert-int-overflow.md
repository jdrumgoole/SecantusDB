### $toInt and $convert enforce int32 / int64 overflow bounds

`$toInt` and `$convert` (to `int` / `long`) never range-checked their result:
`$toInt: 1e30` returned an unbounded Python integer, and a value larger than the
target type silently widened instead of overflowing. mongod errors (241,
"Conversion would overflow target type in $convert") — or routes to `$convert`'s
`onError`. SecantusDB now does the same on both servers.

`$toInt` also now yields an int32 (a plain int on the wire) rather than
preserving an int64 input's type, matching mongod, which always narrows to int.
Non-finite doubles (`inf` / `nan`) overflow rather than raising an uncaught
Python error. The Python server carries mongod's 241 code; the Rust core defers
the overflow cases to `BadValue`. Valid in-range conversions are unaffected.
Three-way mongod 7.0.12-verified.

#### Fixed

- `$toInt` / `$convert` (int/long) error on an out-of-range or non-finite value
  (mongod 241, caught by `$convert`'s `onError`) instead of returning an
  unbounded / silently-widened integer, and `$toInt` narrows an int64 input to
  int32 like mongod (both servers).
