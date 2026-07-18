### $indexOfBytes / $indexOfCP validate their start / end index

The `$indexOfBytes` and `$indexOfCP` operators mishandled a non-integer start /
end index: a whole double (`2.0`) was silently ignored (the whole expression
returned `-1`), and a bool was coerced to an integer. mongod accepts an integer or
a whole double, and rejects everything else: a fractional double, a bool, or a
non-numeric index is `Location40096` ("requires an integral … index"), and a
negative index is `Location40097` ("requires a nonnegative … index"). Both servers
now match.

The Python server carries mongod's codes (reproducing its verbatim missing-space
message quirk); the Rust core defers the invalid cases and now computes a
whole-double index rather than returning `-1`. Three-way mongod 7.0.12-verified.

#### Fixed

- `$indexOfBytes` / `$indexOfCP` accept a whole-double start / end index and reject
  a fractional / bool / non-numeric index (`Location40096`) or a negative one
  (`Location40097`), instead of silently returning `-1` or coercing a bool (both
  servers).
