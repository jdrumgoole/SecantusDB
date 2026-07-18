### $sort stage validates its direction values

The `$sort` aggregation stage didn't validate its spec. A string direction
(`{v: "asc"}`) leaked a raw Python `ValueError`, a bool was silently coerced to
ascending, a numeric value other than ±1 (`0`, `2`) was treated as ascending, and
an empty `{}` spec was a silent no-op. mongod rejects each: a non-numeric
direction is `Location15974` ("Illegal key in $sort specification"), a numeric
non-±1 is `Location15975` ("must be 1 … or -1"), and an empty spec is
`Location15976` ("must have at least one sort key"). A whole double (`1.0`) is
still accepted as ±1. Both servers now match.

The Python server carries mongod's codes; the Rust core defers these cases (bool
included — it no longer coerces `true` to `1`) so the Rust server rejects them too.
Three-way mongod 7.0.12-verified.

#### Fixed

- The `$sort` stage rejects a non-numeric direction (`15974`), a numeric non-±1
  direction (`15975`), and an empty spec (`15976`), instead of leaking a Python
  `ValueError`, coercing a bool, or silently no-op-ing (both servers).
