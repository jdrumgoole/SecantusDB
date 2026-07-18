### Array operators reject a non-array input instead of silently yielding null

`$first`, `$last`, `$reverseArray`, `$concatArrays`, `$slice`, `$map`, `$filter`,
and `$reduce` all silently returned `null` when their input wasn't an array.
mongod errors on a non-array (non-null) input, each with its own code: `$first` /
`$last` `28689`, `$reverseArray` `34435`, `$concatArrays` `28664`, `$slice`
`28724`, `$map` `16883`, `$filter` `28651`, `$reduce` `40080`. A null or missing
input still yields `null`. Both servers now match.

The Python server carries mongod's codes; the Rust core defers a non-array input
(so the Rust server rejects it) and now distinguishes a null input (→ null) from a
non-array one. Three-way mongod 7.0.12-verified.

#### Fixed

- `$first` / `$last` / `$reverseArray` / `$concatArrays` / `$slice` / `$map` /
  `$filter` / `$reduce` reject a non-array input with the operator's mongod code,
  instead of silently returning `null`; a null / missing input still yields `null`
  (both servers).
