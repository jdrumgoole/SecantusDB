### $regex / $options validate their arguments instead of silently ignoring

A `$regex` / `$options` query condition wasn't validated. An unknown option flag
(`{$options: "z"}`) was silently ignored, a non-string `$options` was interpreted
as raw regex flags, `$options` with no sibling `$regex` silently matched, and a
non-string `$regex` value leaked a Python error. mongod rejects each: an unknown
flag is `Location51108` ("invalid flag in regex options: X"), and the other three
are `BadValue` ("$options has to be a string" / "$options needs a $regex" /
"$regex has to be a string"). Both servers now match.

Valid flags (`imsxu`), an empty option string, a plain `$regex` string, and a BSON
regex literal are all unaffected. The Python server carries mongod's codes; the
Rust core defers these cases so the Rust server rejects them too. Three-way mongod
7.0.12-verified.

#### Fixed

- A `$regex` query validates its options: an unknown flag is rejected with
  `Location51108`, and a non-string `$options`, an `$options` without a sibling
  `$regex`, or a non-string `$regex` value with `BadValue` — instead of silently
  ignoring the flag, coercing, matching, or leaking a Python error (both servers).
